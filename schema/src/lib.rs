#![no_std]

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Debug, defmt::Format)]
#[allow(non_snake_case)]
pub enum Command {
    SetFrequency { frequency_kHz: f64 },
}

impl Command {
    pub fn serialize<'a>(&self, buf: &'a mut [u8]) -> Result<&'a mut [u8], postcard::Error> {
        postcard::to_slice(self, buf)
    }

    pub fn deserialize(bs: &[u8]) -> Option<Self> {
        postcard::from_bytes(bs).ok()
    }
}
