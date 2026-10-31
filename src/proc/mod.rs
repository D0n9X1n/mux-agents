//! The process table seam; each OS supplies its own `Os`.

pub trait Os {
    fn is_alive(&self, pid: u32) -> bool;
}
