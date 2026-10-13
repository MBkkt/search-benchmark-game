# AWS half of the bench box's cloud interface — the nightly's original home,
# kept switchable via the workflow's `cloud` input. See cloud-shim-gcp.sh for
# the contract both shims implement.

BENCH_USER="ec2-user"
REGION="us-east-1"
BUCKET="sbg-bench-corpus"
DONE_PARAM="/sbg-bench/done"

cloud_install_deps() {
  # docker is needed by the iresearch (SereneDB) engine, whose build runs in a
  # container (clang-21) — see RUNNING.md. gradle needs unzip; bzip2 is the
  # corpus fallback. Amazon Linux 2023 has all of it in its base repos, and
  # already ships a high nofile limit.
  dnf install -y git make gcc gcc-c++ cmake clang bzip2 python3 unzip wget docker
  systemctl enable --now docker
}

cloud_get() { aws s3 cp "s3://$BUCKET/$1" "$2" --region "$REGION"; }
# Secrets are SSM SecureString parameters under /sbg-bench/, the same
# namespace as the done parameter.
cloud_secret() {
  aws ssm get-parameter --name "/sbg-bench/$1" --with-decryption \
    --query Parameter.Value --output text --region "$REGION" 2>/dev/null
}
cloud_put() { aws s3 cp "$1" "s3://$BUCKET/$2" --region "$REGION"; }

cloud_signal() {
  # Log first, so a failed run is always inspectable; the done parameter is
  # written last and is what the workflow polls for.
  aws s3 cp /var/log/sbg-bench.log "s3://$BUCKET/bench-log.txt" \
    --region "$REGION" 2>/dev/null || true
  aws ssm put-parameter --name "$DONE_PARAM" --value "$1" \
    --type String --overwrite --region "$REGION" 2>/dev/null || true
}
