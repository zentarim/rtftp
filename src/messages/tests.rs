use super::*;

#[test]
fn parse_rrq() {
    let filename = "irrelevant.file";
    let binding = vec![
        RRQ.to_be_bytes().to_vec(),
        filename.as_bytes().to_vec(),
        vec![0x00],
        OCTET.as_bytes().to_vec(),
        vec![0x00],
    ];
    let raw: Vec<u8> = binding.iter().flatten().copied().collect();
    let rrq = ReadRequest::parse(&raw);
    assert!(rrq.is_ok());
}
#[test]
fn parse_incomplete_rrq() {
    let filename = "irrelevant.file";
    let binding = vec![
        RRQ.to_be_bytes().to_vec(),
        filename.as_bytes().to_vec(),
        vec![0x00],
    ];
    let raw: Vec<u8> = binding.iter().flatten().copied().collect();
    let error = ReadRequest::parse(&raw).err().unwrap();
    assert!(error.to_string().contains("Bad format"));
}

#[test]
fn parse_empty_rrq() {
    let error = ReadRequest::parse(&vec![]).err().unwrap();
    assert!(error.to_string().contains("Bad format"));
}
