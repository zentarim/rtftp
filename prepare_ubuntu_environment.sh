#!/bin/bash
set -eux
cd "${0%/*}"

apt-get install -y gcc libguestfs-dev guestfs-tools nbd-client

source /etc/os-release

if [ "${ID}" == "ubuntu" ]; then
  echo "Ubuntu OS found. Add kernel hook to allow 'sudo' group to read kernel image.
See: https://bugs.launchpad.net/ubuntu/+source/linux/+bug/759725"
  POSTINSTALL_SCRIPT="/etc/kernel/postinst.d/zz_kernel_libguestf_perm"
  cat <<'EOF' >${POSTINSTALL_SCRIPT}
#!/bin/sh

IMAGE_PATH="${2}"

if [ -f "${IMAGE_PATH}" ]; then
  echo "Make ${IMAGE_PATH} readable for group 'sudo'"
  chown root:sudo "${IMAGE_PATH}"
  chmod g+r "${IMAGE_PATH}"
else
  echo "WARNING: ${IMAGE_PATH} not found, consider manual preparation of the current kernel or running RTFTP with root privileges" >&2
fi
EOF
chmod u+x "${POSTINSTALL_SCRIPT}"
KERNEL_IMAGE_PATH="$(grep -oP 'BOOT_IMAGE=\K\S+' /proc/cmdline)"
test -f "${KERNEL_IMAGE_PATH}" || KERNEL_IMAGE_PATH="/boot${KERNEL_IMAGE_PATH}"
${POSTINSTALL_SCRIPT} "" "${KERNEL_IMAGE_PATH}"
fi
