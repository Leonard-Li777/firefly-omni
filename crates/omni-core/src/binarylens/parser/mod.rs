pub mod elf_macho;
pub mod pe;

pub use elf_macho::parse_elf_macho;
pub use pe::{parse_pe, PE_RESOURCE_MAX_DEPTH};
