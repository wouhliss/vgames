use std::io::Cursor;

use proptest::prelude::*;

use super::*;

const SECTION_RVA: u32 = 0x1000;
const RAW: usize = 0x200;

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// A minimal PE (PE32+ unless `pe32`) with one section holding the import
/// descriptors for `imports` and the delay-import descriptors for `delayed`.
pub(crate) fn pe(imports: &[&str], delayed: &[&str], pe32: bool) -> Vec<u8> {
    let mut data = Vec::new();
    let names_at = (imports.len() + 1) * 20 + (delayed.len() + 1) * 32;
    let mut names = Vec::new();
    let mut name_rvas = Vec::new();
    for name in imports.iter().chain(delayed) {
        name_rvas.push(SECTION_RVA + u32::try_from(names_at + names.len()).unwrap());
        names.extend_from_slice(name.as_bytes());
        names.push(0);
    }
    for rva in name_rvas.iter().take(imports.len()) {
        let mut d = [0u8; 20];
        put32(&mut d, 12, *rva);
        data.extend_from_slice(&d);
    }
    data.extend_from_slice(&[0; 20]);
    let delay_at = data.len();
    for rva in name_rvas.iter().skip(imports.len()) {
        let mut d = [0u8; 32];
        put32(&mut d, 4, *rva);
        data.extend_from_slice(&d);
    }
    data.extend_from_slice(&[0; 32]);
    data.extend_from_slice(&names);

    let optional_size: u16 = if pe32 { 96 + 128 } else { 112 + 128 };
    let mut b = vec![0u8; RAW];
    b[0] = b'M';
    b[1] = b'Z';
    put32(&mut b, 0x3c, 0x40);
    b[0x40..0x44].copy_from_slice(b"PE\0\0");
    let coff = 0x44;
    put16(&mut b, coff, 0x8664);
    put16(&mut b, coff + 2, 1);
    put16(&mut b, coff + 16, optional_size);
    let optional = coff + 20;
    put16(&mut b, optional, if pe32 { 0x10b } else { 0x20b });
    let dirs = optional + if pe32 { 96 } else { 112 };
    put32(&mut b, dirs - 4, 16);
    put32(&mut b, dirs + 8, SECTION_RVA); // imports
    put32(
        &mut b,
        dirs + 13 * 8,
        SECTION_RVA + u32::try_from(delay_at).unwrap(),
    ); // delay imports
    let section = optional + usize::from(optional_size);
    b[section..section + 6].copy_from_slice(b".idata");
    let len = u32::try_from(data.len()).unwrap();
    put32(&mut b, section + 8, len);
    put32(&mut b, section + 12, SECTION_RVA);
    put32(&mut b, section + 16, len);
    put32(&mut b, section + 20, u32::try_from(RAW).unwrap());
    b.extend_from_slice(&data);
    b
}

fn run(bytes: &[u8]) -> Result<D3dImports, PeError> {
    scan(&mut Cursor::new(bytes), bytes.len() as u64)
}

#[test]
fn direct3d_imports_are_found() {
    let found = run(&pe(&["KERNEL32.dll", "D3D11.dll", "dxgi.dll"], &[], false)).unwrap();
    assert_eq!(
        found,
        D3dImports {
            d3d11: true,
            ..D3dImports::default()
        }
    );
    let found = run(&pe(&["d3d9.dll", "d3d10_1.dll"], &["d3d12.dll"], true)).unwrap();
    assert_eq!(
        found,
        D3dImports {
            d3d9: true,
            d3d10: true,
            d3d12: true,
            d3d11: false
        }
    );
    assert_eq!(
        run(&pe(&["user32.dll"], &[], false)).unwrap(),
        D3dImports::default()
    );
}

#[test]
fn non_pe_and_damaged_headers_are_refused() {
    assert_eq!(run(b""), Err(PeError::NotPe));
    assert_eq!(run(b"#!/bin/sh\nexit 0\n"), Err(PeError::NotPe));
    let mut b = pe(&["d3d12.dll"], &[], false);
    b[0x40] = b'X';
    assert_eq!(run(&b), Err(PeError::NotPe));
    let mut b = pe(&["d3d12.dll"], &[], false);
    put32(&mut b, 0x3c, 0xffff_fff0);
    assert!(matches!(run(&b), Err(PeError::Malformed(_))));
    let mut b = pe(&["d3d12.dll"], &[], false);
    let len = b.len();
    b.truncate(len - 5); // the name loses its terminator
    assert!(run(&b).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = run(&bytes);
    }

    #[test]
    fn mutated_executables_never_panic(
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..12),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut b = pe(&["kernel32.dll", "d3d11.dll"], &["d3d12.dll"], false);
        for (at, byte) in flips {
            let i = at.index(b.len());
            b[i] = byte;
        }
        let _ = run(&b);
        let keep = cut.index(b.len());
        let _ = run(&b[..keep]);
    }
}

/// `VGAMES_PE_SAMPLE=<exe> VGAMES_PE_EXPECT=d3d11|d3d12 cargo test … -- --ignored`
/// (compat-e2e.yml runs it on the cross-built test games).
#[test]
#[ignore = "needs a real Windows executable"]
fn a_real_executable_is_scanned() {
    let path = std::env::var("VGAMES_PE_SAMPLE").unwrap();
    let found = scan_file(std::path::Path::new(&path)).unwrap();
    println!("{path}: {found:?}");
    match std::env::var("VGAMES_PE_EXPECT").unwrap().as_str() {
        "d3d11" => assert!(found.d3d11 && !found.d3d12),
        "d3d12" => assert!(found.d3d12 && !found.d3d11),
        other => panic!("unknown expectation {other}"),
    }
}
