use std::any::type_name;
use std::env;
use std::fs::create_dir;
use std::path::PathBuf;

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
