# GCP half of the bench box's cloud interface.
#
# Spliced into user-data-template.sh at its shim marker and written to
# /run/sbg/cloud-shim.sh, which both the root startup script and the
# unprivileged bench.sh source. Everything cloud-specific about the box lives
# here; the template itself is cloud-neutral. The AWS twin is
# cloud-shim-aws.sh, and the two must keep the same contract:
#
#   BENCH_USER            unprivileged user the bench runs as
#   cloud_install_deps    OS packages + anything the distro needs
#   cloud_get  <name> <local>   fetch an object from the bench bucket
#   cloud_put  <local> <name>   upload an object to the bench bucket
#   cloud_signal <ok|error>     upload the run log, then publish the verdict
#   cloud_secret <name>         print a secret's value; non-zero if unavailable

BENCH_USER="bench"
BUCKET="gs://sbg-bench-corpus"
DONE_OBJECT="$BUCKET/signal/done"

# Secrets live in Secret Manager in the CI project, readable by the box's
# service account. Nothing secret goes through the startup script, which is
# instance metadata.
SECRET_PROJECT="infino-dev-ci"

cloud_secret() {
  gcloud secrets versions access latest --secret="$1" --project="$SECRET_PROJECT" \
    --quiet 2>/dev/null
}

cloud_install_deps() {
  # Rocky 9 (chosen over Debian/Ubuntu because it keeps this dnf line a near
  # twin of the Amazon Linux one, and matches the long-lived GCP perf box).
  dnf install -y git make gcc gcc-c++ cmake clang bzip2 python3 unzip wget \
    dnf-plugins-core

  # docker is needed by the iresearch (SereneDB) engine, whose build runs in a
  # container (clang-21) — see RUNNING.md. Unlike Amazon Linux, Rocky has no
  # docker in its base repos, so it comes from Docker's own.
  dnf config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo
  dnf install -y docker-ce docker-ce-cli containerd.io
  systemctl enable --now docker

  # The GCE Rocky image does not ship the gcloud CLI, and cloud_get/cloud_put
  # below are the only way the corpus and results move.
  cat > /etc/yum.repos.d/google-cloud-sdk.repo <<'REPO_EOF'
[google-cloud-cli]
name=Google Cloud CLI
baseurl=https://packages.cloud.google.com/yum/repos/cloud-sdk-el9-x86_64
enabled=1
gpgcheck=1
repo_gpgcheck=0
gpgkey=https://packages.cloud.google.com/yum/doc/rpm-package-key.gpg
REPO_EOF
  dnf install -y google-cloud-cli

  # Rocky defaults nofile to 1024 and a positional index build dies with
  # TooManyOpenFiles far below that. Amazon Linux already ships a high limit,
  # so this raise is GCP-only. limits.d is read by sudo's PAM session, which
  # is how bench.sh is launched.
  cat > /etc/security/limits.d/99-sbg-bench.conf <<'LIM_EOF'
*  soft  nofile  65535
*  hard  nofile  65535
LIM_EOF
}

cloud_get() { gcloud storage cp "$BUCKET/$1" "$2"; }
cloud_put() { gcloud storage cp "$1" "$BUCKET/$2"; }

cloud_signal() {
  # Log first, so a failed run is always inspectable; the done marker is
  # written last and is what the workflow polls for.
  gcloud storage cp /var/log/sbg-bench.log "$BUCKET/bench-log.txt" 2>/dev/null || true
  printf '%s' "$1" | gcloud storage cp - "$DONE_OBJECT" 2>/dev/null || true
}
