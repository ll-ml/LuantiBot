use anyhow::Result;
use num_bigint::BigUint;
use num_traits::Num;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub struct SrpClient {
    pub username_original: String,
    pub username_lower: String,
    pub password: String,
    a: BigUint,
    pub a_bytes: Vec<u8>,
}

impl SrpClient {
    pub fn new(username: &str, password: &str) -> Result<Self> {
        let n = BigUint::from_str_radix(SRP_N_HEX, 16)?;
        let g = BigUint::from(2u32);

        let mut a_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut a_bytes);
        let a = BigUint::from_bytes_be(&a_bytes);
        let a_pub = g.modpow(&a, &n);

        Ok(Self {
            username_original: username.to_string(),
            username_lower: username.to_lowercase(),
            password: password.to_string(),
            a,
            a_bytes: a_pub.to_bytes_be(),
        })
    }

    pub fn process_challenge(&self, salt: &[u8], b_bytes: &[u8]) -> Result<Vec<u8>> {
        let n = BigUint::from_str_radix(SRP_N_HEX, 16)?;
        let g = BigUint::from(2u32);
        let a_pub = BigUint::from_bytes_be(&self.a_bytes);
        let b = BigUint::from_bytes_be(b_bytes);

        let u = h_nn(&n, &a_pub, &b);
        let x = calculate_x(&self.username_lower, &self.password, salt);
        let k = h_nn(&n, &n, &g);
        let v = g.modpow(&x, &n);

        let kv = (&k * &v) % &n;
        let base = if b >= kv {
            (&b - kv) % &n
        } else {
            (&b + &n - kv) % &n
        };
        let exp = &self.a + (&u * &x);
        let s = base.modpow(&exp, &n);
        let k_bytes = hash_num(&s);

        let m = calculate_m(&self.username_original, salt, &n, &g, &a_pub, &b, &k_bytes);
        Ok(m)
    }
}

pub fn generate_srp_verifier_and_salt(player: &str, password: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut rng = rand::thread_rng();
    let mut salt = [0u8; 16];
    rng.fill_bytes(&mut salt);

    let username = player.to_lowercase();
    let inner = Sha256::digest(format!("{}:{}", username, password));
    let mut outer = Sha256::new();
    outer.update(&salt);
    outer.update(inner);
    let x = BigUint::from_bytes_be(&outer.finalize());

    let n = BigUint::from_str_radix(SRP_N_HEX, 16)?;
    let g = BigUint::from(2u32);
    let v = g.modpow(&x, &n);

    Ok((salt.to_vec(), v.to_bytes_be()))
}

const SRP_N_HEX: &str = concat!(
    "AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC319294",
    "3DB56050A37329CBB4A099ED8193E0757767A13DD52312AB4B03310D",
    "CD7F48A9DA04FD50E8083969EDB767B0CF6095179A163AB3661A05FB",
    "D5FAAAE82918A9962F0B93B855F97993EC975EEAA80D740ADBF4FF74",
    "7359D041D5C33EA71D281E446B14773BCA97B43A23FB801676BD207A",
    "436C6481F1D2B9078717461A5B9D32E688F87748544523B524B0D57D",
    "5EA77A2775D2ECFA032CFBDBF52FB3786160279004E57AE6AF874E73",
    "03CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8E9DBFBB6",
    "94B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F",
    "9E4AFF73",
);

fn hash_bytes(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

fn hash_num(n: &BigUint) -> Vec<u8> {
    hash_bytes(&n.to_bytes_be())
}

fn pad_num(n: &BigUint, len: usize) -> Vec<u8> {
    let bytes = n.to_bytes_be();
    if bytes.len() >= len {
        return bytes;
    }
    let mut out = vec![0u8; len - bytes.len()];
    out.extend_from_slice(&bytes);
    out
}

fn h_nn(n: &BigUint, n1: &BigUint, n2: &BigUint) -> BigUint {
    let len_n = n.to_bytes_be().len();
    let mut buf = Vec::with_capacity(len_n * 2);
    buf.extend_from_slice(&pad_num(n1, len_n));
    buf.extend_from_slice(&pad_num(n2, len_n));
    BigUint::from_bytes_be(&hash_bytes(&buf))
}

fn calculate_x(username: &str, password: &str, salt: &[u8]) -> BigUint {
    let mut inner = Sha256::new();
    inner.update(username.as_bytes());
    inner.update(b":");
    inner.update(password.as_bytes());
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(salt);
    outer.update(inner_hash);
    BigUint::from_bytes_be(&outer.finalize())
}

fn calculate_m(
    username: &str,
    salt: &[u8],
    n: &BigUint,
    g: &BigUint,
    a: &BigUint,
    b: &BigUint,
    k_bytes: &[u8],
) -> Vec<u8> {
    let h_n = hash_num(n);
    let h_g = hash_num(g);
    let mut h_xor = vec![0u8; h_n.len()];
    for i in 0..h_n.len() {
        h_xor[i] = h_n[i] ^ h_g[i];
    }
    let h_i = hash_bytes(username.as_bytes());

    let len_n = n.to_bytes_be().len();
    let mut buf = Vec::new();
    buf.extend_from_slice(&h_xor);
    buf.extend_from_slice(&h_i);
    buf.extend_from_slice(salt);
    buf.extend_from_slice(&pad_num(a, len_n));
    buf.extend_from_slice(&pad_num(b, len_n));
    buf.extend_from_slice(k_bytes);
    hash_bytes(&buf)
}
