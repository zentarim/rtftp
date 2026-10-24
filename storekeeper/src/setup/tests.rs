use super::{is_ramfs_magic, module_entry, mount_options, mounts_from_cmdline, staged_mountpoint};

#[test]
fn parses_json_order_and_root_mount() {
    let cmdline = "console=ttyS0 reboot=k panic=-1 mounts=/dev/vda2:/,/dev/vda1:/boot";
    let mounts = mounts_from_cmdline(cmdline).unwrap();
    assert_eq!(
        mounts,
        vec![
            ("/dev/vda2".to_string(), "/".to_string()),
            ("/dev/vda1".to_string(), "/boot".to_string()),
        ]
    );
}

#[test]
fn empty_mount_list_is_valid() {
    let mounts = mounts_from_cmdline("console=ttyS0 mounts=").unwrap();
    assert!(mounts.is_empty());
}

#[test]
fn rejects_a_mount_entry_without_a_colon() {
    let error = mounts_from_cmdline("mounts=/dev/vda1").unwrap_err();
    assert!(error.contains("colon"));
}

#[test]
fn ext_filesystems_retry_with_noload() {
    assert_eq!(mount_options("ext4"), vec![None, Some("noload")]);
    assert_eq!(mount_options("xfs"), vec![None]);
    assert_eq!(mount_options("vfat"), vec![None]);
}

#[test]
fn module_list_marks_optional_filesystems() {
    assert_eq!(
        module_entry("required /lib/modules/test/virtio_blk.ko"),
        (true, "/lib/modules/test/virtio_blk.ko")
    );
    assert_eq!(
        module_entry("optional /lib/modules/test/ubifs.ko"),
        (false, "/lib/modules/test/ubifs.ko")
    );
    assert_eq!(
        module_entry("/lib/modules/test/vsock.ko"),
        (true, "/lib/modules/test/vsock.ko")
    );
}

#[test]
fn ramfs_magic_matches_the_initramfs_root() {
    assert!(is_ramfs_magic(0x8584_58f6));
    assert!(!is_ramfs_magic(0xef53));
}

#[test]
fn root_is_staged_under_the_mount_directory() {
    assert_eq!(staged_mountpoint("/"), "/mnt");
    assert_eq!(staged_mountpoint("/boot"), "/mnt/boot");
}
