use crate::fs::OpenedFile;
use crate::remote_fs::FileReader;
use std::any::type_name;
use std::env;
use std::fs::{File, create_dir};
use std::path::PathBuf;
use std::process::Command;

const DATA_PATTERN: &str = "ARBITRARY DATA";

fn get_fn_name<T>(_: T) -> &'static str {
    type_name::<T>()
}

pub fn mk_tmp<T>(test_func: T) -> PathBuf {
    let test_dir_name = get_fn_name(test_func).replace("::", "_");
    let pid = std::process::id();
    let test_tmp_dir = env::temp_dir().join(format!("rtftp_{pid}_{test_dir_name}"));
    create_dir(&test_tmp_dir).unwrap();
    test_tmp_dir
}

pub(super) fn read_file(opened: &mut FileReader) -> Vec<u8> {
    let mut buffer = vec![];
    let mut chunk = vec![0u8; 512];
    loop {
        let read_size = opened.read_to(&mut chunk).unwrap();
        if read_size == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read_size]);
    }
    buffer
}

pub(super) fn make_payload(size: usize) -> Vec<u8> {
    let pattern = DATA_PATTERN.as_bytes();
    pattern.iter().copied().cycle().take(size).collect()
}

pub(super) fn ensure_prerequisite_disk() -> (PathBuf, File) {
    let test_data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let test_disk = test_data_dir.join("test_disk.qcow2");
    let file = File::open(&test_data_dir).unwrap();
    file.lock().unwrap();
    if !test_disk.exists() {
        let script = test_data_dir.join("build_test_qcow_disk.sh");
        let status = Command::new(&script)
            .arg(&test_disk)
            .arg(DATA_PATTERN)
            .status()
            .expect(format!("{:?} failed", script).as_str());
        if !status.success() {
            panic!("{script:?} failed");
        }
    }
    (test_disk, file)
}
