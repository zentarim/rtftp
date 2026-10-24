#!/bin/bash
# Build an uncompressed newc cpio for the host kernel. The archive is cached
# and reused until the kernel release, module indexes, or the storekeeper binary is newer.
set -euo pipefail

MODULES_DIR=${MODULES_DIR:-/lib/modules/$(uname -r)}
KERNEL_CONFIG=${KERNEL_CONFIG:-/boot/config-$(uname -r)}
: "${STOREKEEPER:?STOREKEEPER is required}"

if [[ ! -f "$STOREKEEPER" ]]; then
  echo "storekeeper binary is missing: $STOREKEEPER" >&2
  exit 1
fi
if [[ ! -f "$KERNEL_CONFIG" ]]; then
  echo "kernel config is missing: $KERNEL_CONFIG" >&2
  exit 1
fi
if [[ ! -f "${MODULES_DIR}/modules.dep" ]]; then
  echo "modules.dep is missing: ${MODULES_DIR}/modules.dep" >&2
  exit 1
fi

uid=$(id -u)
OUTPUT=${OUTPUT:-/var/tmp/rtftp-${uid}/initramfs.cpio}
mkdir -p "$(dirname "$OUTPUT")"
exec 9>"${OUTPUT}.lock"
flock 9

release=$(basename "$MODULES_DIR")
modules_dep="${MODULES_DIR}/modules.dep"
modules_alias="${MODULES_DIR}/modules.alias"
modules_softdep="${MODULES_DIR}/modules.softdep"
script_self=$(readlink -f "$0")
stamp="${OUTPUT}.stamp"
debug() {
  printf 'mk-initramfs: %s\n' "$*" >&2
}
cache_decision() {
  if [[ ! -f "$OUTPUT" || ! -f "$stamp" ]]; then
    echo "no cached archive"
    return
  fi
  if [[ "$(cat "$stamp")" != "$release" ]]; then
    echo "kernel release changed"
    return
  fi
  if [[ "$modules_dep" -nt "$OUTPUT" ]]; then
    echo "modules.dep is newer"
    return
  fi
  if [[ -f "$modules_alias" && "$modules_alias" -nt "$OUTPUT" ]]; then
    echo "modules.alias is newer"
    return
  fi
  if [[ -f "$modules_softdep" && "$modules_softdep" -nt "$OUTPUT" ]]; then
    echo "modules.softdep is newer"
    return
  fi
  if [[ "$STOREKEEPER" -nt "$OUTPUT" ]]; then
    echo "storekeeper is newer"
    return
  fi
  if [[ "$script_self" -nt "$OUTPUT" ]]; then
    echo "builder script is newer"
    return
  fi
  echo "cache is current"
}
decision=$(cache_decision)
if [[ "$decision" == "cache is current" ]]; then
  debug "reusing ${OUTPUT} for ${release} (${decision}, script ${script_self})"
  exit 0
fi
debug "building ${OUTPUT} for ${release} (${decision}, script ${script_self})"

config_get() {
  local key="$1"
  local line
  line=$(grep -E "^${key}=" "$KERNEL_CONFIG" | head -n 1 || true)
  if [[ -z "$line" ]]; then
    echo unset
    return
  fi
  echo "${line#*=}"
}

find_module() {
  local name="$1"
  find "$MODULES_DIR" -type f \
    \( -name "${name}.ko" -o -name "${name}.ko.gz" -o -name "${name}.ko.xz" -o -name "${name}.ko.zst" \) \
    -print -quit
}

require_feature() {
  local config_key="$1"
  local module_name="$2"
  local value
  value=$(config_get "$config_key")
  local found
  found=$(find_module "$module_name" || true)
  case "$value" in
    y)
      if [[ -n "$found" ]]; then
        printf '%s\n' "$found"
      fi
      ;;
    m)
      if [[ -z "$found" ]]; then
        echo "${config_key}=m but ${module_name} was not found under ${MODULES_DIR}" >&2
        exit 1
      fi
      printf '%s\n' "$found"
      ;;
    *)
      echo "${config_key} is ${value}; expected y or m" >&2
      exit 1
      ;;
  esac
}

relative_module() {
  local path="$1"
  local relative="${path#"$MODULES_DIR"/}"
  if [[ "$relative" == "$path" ]]; then
    echo "module is outside ${MODULES_DIR}: ${path}" >&2
    exit 1
  fi
  printf '%s\n' "$relative"
}

module_name_of() {
  local base
  base=$(basename "$1")
  base="${base%.zst}"
  base="${base%.xz}"
  base="${base%.gz}"
  base="${base%.ko}"
  printf '%s\n' "${base//-/_}"
}

skipped_filesystem() {
  case "$1" in
    */kernel/fs/nfs/* | */kernel/fs/nfsd/* | */kernel/fs/cifs/* | */kernel/fs/smb/* | \
      */kernel/fs/9p/* | */kernel/fs/ceph/* | */kernel/fs/gfs2/* | */kernel/fs/ocfs2/* | \
      */kernel/fs/afs/* | */kernel/fs/fuse/* | */kernel/fs/overlayfs/* | */kernel/fs/autofs/* | \
      */kernel/fs/virtiofs/* | */kernel/fs/pstore/* | */kernel/fs/binfmt_misc/*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

declare -A DEP_OF=()
pending=""
while IFS= read -r line || [[ -n "$line" ]]; do
  if [[ "$line" == *\\ ]]; then
    pending+="${line%\\} "
    continue
  fi
  line="${pending}${line}"
  pending=""
  [[ -z "$line" ]] && continue
  module=${line%%:*}
  deps=${line#*:}
  if [[ "$deps" == "$line" ]]; then
    deps=""
  fi
  DEP_OF["$module"]="$deps"
done <"$modules_dep"

declare -A NAME_OF=()
for module in "${!DEP_OF[@]}"; do
  NAME_OF["$(module_name_of "$module")"]="$module"
done

# depmod records "softdep libcrc32c pre: crc32c". modprobe follows that to
# "alias crypto-crc32c crc32c_intel". The guest has no modprobe, so the
# provider modules have to be listed first. The same strings are stored in
# the module image (ext4 uses "pre: crypto-crc32c"); modules.softdep is not
# the only copy.
declare -A ALIAS_TARGETS=()
if [[ -f "$modules_alias" ]]; then
  while read -r alias_word alias_name module_name _; do
    [[ "$alias_word" == alias ]] || continue
    [[ -n "${alias_name:-}" && -n "${module_name:-}" ]] || continue
    ALIAS_TARGETS["$alias_name"]+=" $module_name"
  done <"$modules_alias"
fi

declare -A SOFT_PRE=()
if [[ -f "$modules_softdep" ]]; then
  while read -r softdep_word module_name rest; do
    [[ "$softdep_word" == softdep ]] || continue
    [[ -n "${module_name:-}" ]] || continue
    pre=""
    if [[ "${rest:-}" == *"pre:"* ]]; then
      pre=${rest#*pre:}
      pre=${pre%%post:*}
    elif [[ "${rest:-}" != *"post:"* ]]; then
      pre=$rest
    fi
    module_name="${module_name//-/_}"
    SOFT_PRE["$module_name"]+=" $pre"
  done <"$modules_softdep"
fi

debug "modules.alias $([[ -f "$modules_alias" ]] && echo present || echo missing)"
debug "modules.softdep $([[ -f "$modules_softdep" ]] && echo present || echo missing)"
if command -v modinfo >/dev/null 2>&1; then
  debug "modinfo $(command -v modinfo)"
else
  debug "modinfo missing"
fi
for provider_name in crc32c_generic crc32c_intel crc32c_cryptoapi crc32_generic crc32_pclmul crc32_intel crc32_cryptoapi libcrc32c ext4 jbd2; do
  if [[ -n "${NAME_OF[$provider_name]:-}" ]]; then
    debug "installed ${provider_name} ${NAME_OF[$provider_name]}"
  else
    debug "installed ${provider_name} <absent>"
  fi
done

module_paths_for() {
  local token="$1"
  local -A seen=()
  emit_name() {
    local candidate="${1//-/_}"
    local path="${NAME_OF[$candidate]:-}"
    if [[ -n "$path" && -z "${seen[$path]:-}" ]]; then
      seen[$path]=1
      printf '%s\n' "$path"
    fi
  }
  emit_name "$token"
  local alias_name target
  for alias_name in "$token" "crypto-${token}"; do
    for target in ${ALIAS_TARGETS[$alias_name]:-}; do
      emit_name "$target"
    done
  done
  # Alias indexes occasionally fail to name the installed provider. The
  # filename is stable: crc32c-intel.ko is the module crc32c_intel.
  local fallback=""
  case "$token" in
    crc32c | crypto-crc32c)
      fallback="crc32c_generic crc32c_intel crc32c_cryptoapi"
      ;;
    crc32 | crypto-crc32)
      fallback="crc32_generic crc32_pclmul crc32_intel crc32_cryptoapi"
      ;;
  esac
  local name
  for name in $fallback; do
    emit_name "$name"
  done
}

read_module_softdeps() {
  local src="$1"
  if command -v modinfo >/dev/null 2>&1; then
    local values
    if values=$(modinfo -F softdep -- "$src" 2>/dev/null); then
      printf '%s\n' "$values"
      return
    fi
  fi
  strings -a -- "$src" 2>/dev/null | sed -n 's/^softdep=//p' || true
}

declare -A VISITING=()
declare -A DONE=()
declare -A REQUIRED=()
ORDER=()
walk() {
  local module="$1"
  local required="$2"
  if [[ "$required" == 1 ]]; then
    REQUIRED[$module]=1
  fi
  if [[ -n "${DONE[$module]:-}" ]]; then
    return
  fi
  if [[ -n "${VISITING[$module]:-}" ]]; then
    echo "module dependency cycle: $module" >&2
    exit 1
  fi
  VISITING[$module]=1
  local modname token provider
  modname=$(module_name_of "$module")
  for token in ${SOFT_PRE[$modname]:-}; do
    local providers=()
    while IFS= read -r provider; do
      [[ -n "$provider" ]] && providers+=("$provider")
    done < <(module_paths_for "$token")
    if [[ ${#providers[@]} -eq 0 ]]; then
      echo "softdep ${modname} pre: ${token} did not match an installed module" >&2
    elif [[ "$token" == *crc* || "$modname" == *crc* ]]; then
      debug "resolve ${modname} pre:${token} -> ${providers[*]}"
    fi
    for provider in "${providers[@]+"${providers[@]}"}"; do
      walk "$provider" 0
    done
  done
  local dep
  for dep in ${DEP_OF[$module]:-}; do
    walk "$dep" "$required"
  done
  unset "VISITING[$module]"
  DONE[$module]=1
  ORDER+=("$module")
}

required_seeds=()
while IFS= read -r path; do
  [[ -n "$path" ]] && required_seeds+=("$path")
done < <(require_feature CONFIG_VIRTIO_BLK virtio_blk)
while IFS= read -r path; do
  [[ -n "$path" ]] && required_seeds+=("$path")
done < <(require_feature CONFIG_VIRTIO_VSOCKETS vmw_vsock_virtio_transport)

optional_seeds=()
if [[ -d "${MODULES_DIR}/kernel/block/partitions" ]]; then
  while IFS= read -r path; do
    optional_seeds+=("$path")
  done < <(find "${MODULES_DIR}/kernel/block/partitions" -type f \
    \( -name '*.ko' -o -name '*.ko.gz' -o -name '*.ko.xz' -o -name '*.ko.zst' \))
fi

if [[ -d "${MODULES_DIR}/kernel/fs" ]]; then
  while IFS= read -r path; do
    if skipped_filesystem "$path"; then
      continue
    fi
    optional_seeds+=("$path")
  done < <(find "${MODULES_DIR}/kernel/fs" -type f \
    \( -name '*.ko' -o -name '*.ko.gz' -o -name '*.ko.xz' -o -name '*.ko.zst' \))
fi

REQUIRED_RELS=()
OPTIONAL_RELS=()
declare -A SEEDED=()
for path in "${required_seeds[@]+"${required_seeds[@]}"}"; do
  relative=$(relative_module "$path")
  if [[ -n "${SEEDED[$relative]:-}" ]]; then
    continue
  fi
  SEEDED[$relative]=1
  REQUIRED_RELS+=("$relative")
done
for path in "${optional_seeds[@]+"${optional_seeds[@]}"}"; do
  relative=$(relative_module "$path")
  if [[ -n "${SEEDED[$relative]:-}" ]]; then
    continue
  fi
  SEEDED[$relative]=1
  OPTIONAL_RELS+=("$relative")
done

walk_seeds() {
  VISITING=()
  DONE=()
  REQUIRED=()
  ORDER=()
  local relative
  for relative in "${REQUIRED_RELS[@]+"${REQUIRED_RELS[@]}"}"; do
    walk "$relative" 1
  done
  for relative in "${OPTIONAL_RELS[@]+"${OPTIONAL_RELS[@]}"}"; do
    walk "$relative" 0
  done
}

# Module images carry the softdep strings even when modules.softdep does not
# name them. Providers discovered that way have to be ordered on a later walk,
# ahead of the module that imports them.
declare -A SCANNED=()
absorb_image_softdeps() {
  local module src line modname pre token provider found_new=0
  for module in "${ORDER[@]+"${ORDER[@]}"}"; do
    if [[ -n "${SCANNED[$module]:-}" ]]; then
      continue
    fi
    SCANNED[$module]=1
    src="${MODULES_DIR}/${module}"
    [[ -f "$src" ]] || continue
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      pre=""
      if [[ "$line" == *"pre:"* ]]; then
        pre=${line#*pre:}
        pre=${pre%%post:*}
      elif [[ "$line" != *"post:"* ]]; then
        pre=$line
      fi
      [[ -n "${pre// /}" ]] || continue
      modname=$(module_name_of "$module")
      if [[ "$pre" == *crc* || "$modname" == *crc* || "$modname" == ext4 || "$modname" == jbd2 || "$modname" == f2fs ]]; then
        debug "image softdep ${modname} from ${module}: ${line}"
      fi
      SOFT_PRE["$modname"]+=" $pre"
      for token in $pre; do
        while IFS= read -r provider; do
          [[ -n "$provider" ]] || continue
          if [[ -z "${DONE[$provider]:-}" ]]; then
            found_new=1
          fi
        done < <(module_paths_for "$token")
      done
    done < <(read_module_softdeps "$src")
  done
  if [[ "$found_new" == 1 ]]; then
    return 0
  fi
  return 1
}

for _pass in 1 2 3 4 5 6; do
  walk_seeds
  debug "walk pass ${_pass}: ${#ORDER[@]} modules"
  if absorb_image_softdeps; then
    debug "image softdeps added providers; walking again"
    continue
  fi
  break
done
order_index=0
for module in "${ORDER[@]+"${ORDER[@]}"}"; do
  case "$module" in
    *crc* | *ext4* | *jbd2* | *btrfs* | *f2fs* | *xfs* | *erofs*)
      debug "order ${order_index} ${module}"
      ;;
  esac
  order_index=$((order_index + 1))
done

stage=$(mktemp -d)
cleanup() {
  rm -rf "$stage"
}
trap cleanup EXIT
install -m 0755 "$STOREKEEPER" "${stage}/init"
: >"${stage}/modules.list"

declare -A COPIED_NAME=()
for module in "${ORDER[@]+"${ORDER[@]}"}"; do
  src="${MODULES_DIR}/${module}"
  uncompressed="${module%.gz}"
  uncompressed="${uncompressed%.xz}"
  uncompressed="${uncompressed%.zst}"
  dest="${stage}/lib/modules/${release}/${uncompressed}"
  mkdir -p "$(dirname "$dest")"
  case "$src" in
    *.ko.zst) zstd -d -c -- "$src" >"$dest" ;;
    *.ko.xz) xz -d -c -- "$src" >"$dest" ;;
    *.ko.gz) gzip -d -c -- "$src" >"$dest" ;;
    *.ko) cp -- "$src" "$dest" ;;
    *)
      echo "unsupported module compression: $src" >&2
      exit 1
      ;;
  esac
  if [[ -n "${REQUIRED[$module]:-}" ]]; then
    echo "required /lib/modules/${release}/${uncompressed}" >>"${stage}/modules.list"
  else
    echo "optional /lib/modules/${release}/${uncompressed}" >>"${stage}/modules.list"
  fi
  base=$(basename "$uncompressed")
  base=${base%.ko}
  COPIED_NAME["$base"]=1
done

crc_names=()
needs_crc32c=0
has_crc32c_provider=0
for base in "${!COPIED_NAME[@]}"; do
  case "$base" in
    *crc*) crc_names+=("$base") ;;
  esac
  case "$base" in
    libcrc32c | ext4 | jbd2) needs_crc32c=1 ;;
  esac
  case "$base" in
    crc32c_generic | crc32c-generic | crc32c_intel | crc32c-intel | crc32c_cryptoapi | crc32c-cryptoapi)
      has_crc32c_provider=1
      ;;
  esac
done
if ((${#crc_names[@]} > 0)); then
  debug "packed crc: $(printf '%s\n' "${crc_names[@]}" | sort | paste -sd ' ' -)"
else
  debug "packed crc: none"
fi
if [[ "$needs_crc32c" == 1 && "$has_crc32c_provider" == 0 ]]; then
  debug "no crc32c provider packed; libcrc32c and ext4 cannot register crc32c"
fi

skip_type() {
  case "$1" in
    nfs | nfsd | cifs | smb3 | 9p | afs | kafs | ceph | gfs2 | ocfs2 | fuse | virtiofs | overlay | \
      autofs | autofs4 | binfmt_misc | debugfs | tracefs | securityfs | sysfs | proc | tmpfs | \
      ramfs | devtmpfs | devpts | cgroup | cgroup2 | bpf | pstore | efivarfs | configfs | \
      hugetlbfs | mqueue | rpc_pipefs | fusectl)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

declare -A FOUND_TYPES=()
if [[ -f "${MODULES_DIR}/modules.alias" ]]; then
  while read -r alias_word fs_alias module_name _; do
    [[ "$alias_word" == "alias" ]] || continue
    [[ "$fs_alias" == fs-* ]] || continue
    fstype=${fs_alias#fs-}
    if [[ -n "${COPIED_NAME[$module_name]:-}" ]]; then
      FOUND_TYPES["$fstype"]=1
    fi
  done <"${MODULES_DIR}/modules.alias"
fi

preferred=(ext4 ext3 ext2 xfs btrfs vfat)
{
  printf '%s\n' "${preferred[@]}"
  extras=()
  for fstype in "${!FOUND_TYPES[@]}"; do
    local_preferred=0
    for name in "${preferred[@]}"; do
      if [[ "$fstype" == "$name" ]]; then
        local_preferred=1
      fi
    done
    if [[ "$local_preferred" -eq 1 ]] || skip_type "$fstype"; then
      continue
    fi
    extras+=("$fstype")
  done
  if ((${#extras[@]} > 0)); then
    printf '%s\n' "${extras[@]}" | sort
  fi
} >"${stage}/filesystems.list"

partial="${OUTPUT}.partial"
(
  cd "$stage"
  find . -mindepth 1 -print0 | sed -z 's|^\./||' | cpio --null -o -H newc --quiet
) >"$partial"
echo "$release" >"${stamp}.partial"
mv "$partial" "$OUTPUT"
mv "${stamp}.partial" "$stamp"
