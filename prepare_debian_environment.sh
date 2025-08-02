#!/bin/bash
set -eux
cd "${0%/*}"

apt install -y gcc libguestfs-dev guestfs-tools nbd-client

source /etc/os-release

if [ "${ID}" == "ubuntu" ]; then
  echo "Ubuntu OS found. Add kernel hook to allow 'sudo' group to read kernel image.
See: https://bugs.launchpad.net/ubuntu/+source/linux/+bug/759725"
  cat <<'EOF' >/etc/kernel/postinst.d/zz_kernel_libuestf_perm
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
chmod u+x /etc/kernel/postinst.d/zz_kernel_libuestf_perm
/etc/kernel/postinst.d/zz_kernel_libuestf_perm "" "$(grep -oP 'BOOT_IMAGE=\K\S+' /proc/cmdline)"
fi
