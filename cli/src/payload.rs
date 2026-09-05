pub const DRIVER_SYS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/WispDisk.sys"));
pub const DRIVER_INF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/WispDisk.inf"));
pub const DRIVER_CAT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/WispDisk.cat"));

pub fn package_is_embedded() -> bool {
    env!("WISPDISK_PACKAGE_EMBEDDED") == "1"
}

const _: usize = DRIVER_SYS.len() + DRIVER_INF.len() + DRIVER_CAT.len();
