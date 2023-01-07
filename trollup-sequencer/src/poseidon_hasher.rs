use crate::merkle_tree::Hasher;

use ethers::types::U256;
use poseidon::*;
use zokrates_field::Bn128Field;

#[derive(Default, Clone)]
pub struct PoseidonHasher(Vec<Bn128Field>);

impl Hasher for PoseidonHasher {
    fn write_h256(&mut self, w: &U256) {
        self.0.push(w.to_bn128_field())
    }

    fn finish(self) -> U256 {
        poseidon::hash_BN_128(self.0).to_u256()
    }
}

pub trait ToU256 {
    fn to_u256(&self) -> U256;
}

impl ToU256 for Bn128Field {
    fn to_u256(&self) -> U256 {
        let mut bytes: [u8; 32] = self.to_byte_vector().try_into().unwrap();
        bytes.reverse();
        bytes.into()
    }
}

pub trait ToBn128Field {
    fn to_bn128_field(&self) -> Bn128Field;
}

impl ToBn128Field for U256 {
    fn to_bn128_field(&self) -> Bn128Field {
        let mut n_bytes = vec![0; 32];
        self.to_little_endian(&mut n_bytes);
        Bn128Field::from_byte_vector(n_bytes)
    }
}