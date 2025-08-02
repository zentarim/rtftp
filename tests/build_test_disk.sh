#!/bin/bash
set -eux
cd "${0%/*}"

QCOW_DISK_PATH="${1:?Must be a desirable QCOW2 test disk path}"
DATA_PATTERN="${2:?Must be a data pattern to fill files inside the disk}"

rm -f "${QCOW_DISK_PATH}"
guestfish <<EOF
  disk-create ${QCOW_DISK_PATH} qcow2 1073741824 preallocation:off
  add ${QCOW_DISK_PATH}
  run
  part-init /dev/sda mbr
  part-add /dev/sda p 2048 1048575
  part-add /dev/sda p 1048576 -1
  mkfs vfat /dev/sda1 label:boot
  mkfs ext4 /dev/sda2 label:root
  mount /dev/sda2 /
  mkdir /boot
  mount /dev/sda1 /boot
  fill-pattern '${DATA_PATTERN}' 51200 /boot/aligned.file
  fill-pattern '${DATA_PATTERN}' 51205 /boot/nonaligned.file
EOF
