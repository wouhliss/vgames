//! Controllers (A2-T12, 07-controllers): layout mapping and the per-launch
//! emulation decision. The SDL input thread and the virtual-pad backends
//! build on these.

pub mod decision;
pub mod input;
pub mod mapping;
pub mod navigation;

#[cfg(test)]
mod linkage_tests {
    #[test]
    fn bundled_sdl3_links_and_reports_its_version() {
        // This calls a native symbol: the three-OS desktop matrix must link SDL,
        // even before the input service starts using it in production.
        assert_eq!(sdl3::version::version().major, 3);
    }
}
