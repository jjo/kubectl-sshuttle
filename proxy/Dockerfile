# Pre-baked sshuttle image. The default kubectl-sshuttle flow used to
# `apt-get install` + `pip install sshuttle` at pod startup, which:
#   1. requires root in the pod
#   2. pulls in network at first start (slow + flaky on locked-down VPCs)
#   3. needs an `apt-get` that supports the python image's debian release
#
# This image bakes sshuttle in and runs as a non-root user (uid=1001).

FROM python:3.12-slim
RUN pip install --no-cache-dir sshuttle && \
    groupadd -r -g 1001 sshuttle && \
    useradd  -r -u 1001 -g sshuttle -m -d /home/sshuttle sshuttle
USER sshuttle
WORKDIR /home/sshuttle

# Default: idle so kubectl exec can attach. The actual sshuttle server is
# launched per-session by the local sshuttle (or rushtle) client.
CMD ["sh", "-c", "touch /tmp/ready && exec sleep infinity"]
