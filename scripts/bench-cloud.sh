#!/bin/bash
# Runner-side cloud interface for the nightly bench workflow.
#
# The nightly runs on GCP by default and can be switched back to AWS with the
# workflow's `cloud` input. Rather than duplicate every step in the workflow
# under two `if:` guards, the workflow calls the verbs below and this script
# dispatches on $BENCH_CLOUD. Only the credential step stays in YAML, because
# each cloud's auth action is different.
#
# Verbs:
#   build-user-data <out>      splice the matching box shim + run params in
#   clear-signal               drop a done-signal left by a failed run
#   launch <user-data-file>    create the bench box, print its handle
#   wait                       poll the done-signal until ok/error
#   fetch <name> <local>       download an object from the bench bucket
#   fetch-log                  print the box's run log (best effort)
#   terminate <handle>         delete the bench box
#
# The box-side half of this interface is scripts/cloud-shim-{gcp,aws}.sh.
set -euo pipefail

CLOUD="${BENCH_CLOUD:?BENCH_CLOUD must be set to gcp or aws}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --- GCP ---------------------------------------------------------------
# c3-highcpu-8 is the Sapphire Rapids twin of the AWS c7i.2xlarge: 8 vCPU,
# 16 GiB, same CPU flags. It is also what the long-lived GCP perf box runs,
# so hand-run and nightly numbers stay comparable.
GCP_PROJECT="infino-dev-ci"
GCP_ZONE="us-central1-a"
GCP_MACHINE="c3-highcpu-8"
GCP_VM_SA="sbg-bench-vm@infino-dev-ci.iam.gserviceaccount.com"
GCP_BUCKET="gs://sbg-bench-corpus"
GCP_DONE="$GCP_BUCKET/signal/done"

# --- AWS ---------------------------------------------------------------
AWS_REGION="us-east-1"
AWS_BUCKET="s3://sbg-bench-corpus"
AWS_DONE_PARAM="/sbg-bench/done"

die() { echo "::error::$*" >&2; exit 1; }

build_user_data() {
  local out="$1" shim="$HERE/cloud-shim-${CLOUD}.sh"
  [ -f "$shim" ] || die "no shim for cloud '$CLOUD'"
  : "${INFINO_BRANCH:?}" "${INFINO_REPO:?}" "${SBG_BRANCH:?}" "${SAME_BOX:?}" "${GH_TOKEN:?}"
  # Splice the shim in first, then substitute the run parameters. awk rather
  # than `sed r`, whose filename handling differs between GNU and BSD sed —
  # this script is run by hand on a Mac as well as by the Linux runner.
  awk -v shim="$shim" '$0 == "__CLOUD_SHIM__" {
         while ((getline line < shim) > 0) print line
         next
       } { print }' "$HERE/user-data-template.sh" \
  | sed \
      -e "s|__GH_TOKEN__|${GH_TOKEN}|g" \
      -e "s|__INFINO_BRANCH__|${INFINO_BRANCH}|g" \
      -e "s|__INFINO_REPO__|${INFINO_REPO}|g" \
      -e "s|__SBG_BRANCH__|${SBG_BRANCH}|g" \
      -e "s|__SAME_BOX__|${SAME_BOX}|g" \
      -e "s|__PLATFORM_ONLY__|${PLATFORM_ONLY:-false}|g" \
    > "$out"
  grep -q '__CLOUD_SHIM__' "$out" && die "shim was not spliced in"
  bash -n "$out" || die "generated user-data is not valid bash"
}

clear_signal() {
  case "$CLOUD" in
    gcp) gcloud storage rm "$GCP_DONE" --quiet 2>/dev/null || true ;;
    aws) aws ssm delete-parameter --name "$AWS_DONE_PARAM" --region "$AWS_REGION" 2>/dev/null || true ;;
  esac
}

launch() {
  local ud="$1"
  case "$CLOUD" in
    gcp)
      local vm="sbg-bench-${GITHUB_RUN_ID:-manual}"
      # Egress to github/crates.io/docker is via an ephemeral external IP on
      # the default network — the same path the long-lived perf box uses.
      gcloud compute instances create "$vm" \
        --project "$GCP_PROJECT" --zone "$GCP_ZONE" \
        --machine-type "$GCP_MACHINE" \
        --image-family rocky-linux-9 --image-project rocky-linux-cloud \
        --boot-disk-size 120GB --boot-disk-type pd-balanced \
        --service-account "$GCP_VM_SA" \
        --scopes https://www.googleapis.com/auth/cloud-platform \
        --metadata-from-file "startup-script=$ud" \
        --labels "purpose=sbg-bench-nightly" \
        --quiet >&2
      echo "$vm"
      ;;
    aws)
      local ami sg vpc
      ami=$(aws ec2 describe-images --owners amazon \
        --filters "Name=name,Values=al2023-ami-*-x86_64" "Name=state,Values=available" \
        --query 'sort_by(Images, &CreationDate)[-1].ImageId' --output text)
      vpc=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" \
        --query 'Vpcs[0].VpcId' --output text)
      sg=$(aws ec2 describe-security-groups \
        --filters "Name=vpc-id,Values=$vpc" "Name=group-name,Values=default" \
        --query 'SecurityGroups[0].GroupId' --output text)
      aws ec2 run-instances \
        --image-id "$ami" --instance-type c7i.2xlarge \
        --iam-instance-profile Name=sbg-bench-instance \
        --security-group-ids "$sg" \
        --user-data "$(base64 -w0 "$ud")" \
        --block-device-mappings '[{"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":120,"VolumeType":"gp3"}}]' \
        --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=sbg-bench-nightly}]' \
        --query 'Instances[0].InstanceId' --output text
      ;;
  esac
}

# Read the done-signal. Echoes ok / error / pending, or exits non-zero if the
# read itself failed. Distinguishing "not published yet" from a real API
# failure matters: masking the latter spins the poll to its timeout on expired
# credentials despite a perfectly good bench.
read_signal() {
  local out
  case "$CLOUD" in
    gcp)
      if out=$(gcloud storage cat "$GCP_DONE" 2>&1); then
        echo "$out"
      elif printf '%s' "$out" | grep -qiE "not found|no url|matched no objects|404"; then
        echo pending
      else
        echo "reading the done-signal failed (not a missing object):" >&2
        echo "$out" >&2; return 1
      fi
      ;;
    aws)
      if out=$(aws ssm get-parameter --name "$AWS_DONE_PARAM" \
                 --query 'Parameter.Value' --output text --region "$AWS_REGION" 2>&1); then
        echo "$out"
      elif printf '%s' "$out" | grep -q "ParameterNotFound"; then
        echo pending
      else
        echo "get-parameter failed (not a missing parameter):" >&2
        echo "$out" >&2; return 1
      fi
      ;;
  esac
}

wait_for_bench() {
  echo "Polling the done-signal every 60s …"
  while true; do
    local status; status=$(read_signal) || exit 1
    echo "$(date -u +%H:%M:%S)  status=$status"
    [ "$status" = "ok" ] && break
    [ "$status" = "error" ] && die "Bench reported error on the box."
    sleep 60
  done
}

fetch() {
  case "$CLOUD" in
    gcp) gcloud storage cp "$GCP_BUCKET/$1" "$2" ;;
    aws) aws s3 cp "$AWS_BUCKET/$1" "$2" --region "$AWS_REGION" ;;
  esac
}

fetch_log() {
  case "$CLOUD" in
    gcp) gcloud storage cat "$GCP_BUCKET/bench-log.txt" 2>/dev/null || echo "Failed to fetch log from GCS" ;;
    aws) aws s3 cp "$AWS_BUCKET/bench-log.txt" - --region "$AWS_REGION" 2>/dev/null || echo "Failed to fetch log from S3" ;;
  esac
}

terminate() {
  local handle="$1"
  [ -n "$handle" ] || { echo "no instance handle — nothing to terminate"; return 0; }
  case "$CLOUD" in
    gcp) gcloud compute instances delete "$handle" --project "$GCP_PROJECT" --zone "$GCP_ZONE" --quiet ;;
    aws) aws ec2 terminate-instances --instance-ids "$handle" --region "$AWS_REGION" >/dev/null ;;
  esac
  echo "Terminated $handle"
}

case "${1:?usage: bench-cloud.sh <verb> [args]}" in
  build-user-data) build_user_data "$2" ;;
  clear-signal)    clear_signal ;;
  launch)          launch "$2" ;;
  wait)            wait_for_bench ;;
  fetch)           fetch "$2" "$3" ;;
  fetch-log)       fetch_log ;;
  terminate)       terminate "${2:-}" ;;
  *) die "unknown verb: $1" ;;
esac
