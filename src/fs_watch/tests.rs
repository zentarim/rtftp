use super::*;
use std::any::type_name;
use std::env;
use std::fs::{create_dir, remove_file};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tokio::runtime::Builder;
use tokio::task::LocalSet;
use tokio::time::timeout;

fn get_fn_name<T>(_: T) -> &'static str {
    type_name::<T>()
}

fn mk_tmp<T>(test_func: T) -> PathBuf {
    let test_dir_name = get_fn_name(test_func).replace("::", "_");
    let pid = std::process::id();
    let test_tmp_dir = env::temp_dir().join(format!("rtftp_{pid}_{test_dir_name}"));
    create_dir(&test_tmp_dir).unwrap();
    test_tmp_dir
}

#[test]
fn test_create_delete() {
    LocalSet::new().block_on(
        &Builder::new_current_thread().enable_all().build().unwrap(),
        test_create_delete_coro(),
    );
}

async fn test_create_delete_coro() {
    let temp_dir = mk_tmp(test_create_delete);
    let watch = Watch::new()
        .change()
        .removal()
        .observe(temp_dir.to_str().unwrap())
        .unwrap();
    let first_path = temp_dir.join("first_file");
    let mut fd = File::create(&first_path).unwrap();
    fd.write(b"Arbitrary payload").unwrap();
    drop(fd);
    remove_file(&first_path).unwrap();
    let second_path = temp_dir.join("second_file");
    let mut fd = File::create(&second_path).unwrap();
    fd.write(b"Arbitrary payload").unwrap();
    drop(fd);
    remove_file(&second_path).unwrap();
    let events = vec![
        watch.next().await,
        watch.next().await,
        watch.next().await,
        watch.next().await,
    ];
    let mut file_names: Vec<_> = events.iter().map(|event| event.file_name()).collect();
    file_names.sort();
    let mut event_actions: Vec<_> = events
        .iter()
        .map(|event| (event.is_modify(), event.is_removal()))
        .collect();
    event_actions.sort();
    assert_eq!(
        file_names,
        vec!["first_file", "first_file", "second_file", "second_file"]
    );
    assert_eq!(
        event_actions,
        vec![(false, true), (false, true), (true, false), (true, false)]
    );
    assert!(timeout(Duration::from_secs(1), watch.next()).await.is_err());
}
