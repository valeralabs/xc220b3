use core::{str, iter::repeat};
use crypto::{chacha20::ChaCha20, symmetriccipher::SynchronousStreamCipher};
use k256::{ecdh::EphemeralSecret, EncodedPoint, PublicKey};
use rand::{thread_rng, Rng, CryptoRng, RngCore};
use tracing::{info, error, debug, info_span};

struct Session {
    ready: bool,
    secret: EphemeralSecret,
    pk: EncodedPoint,
    key: [u8; 32],
    cc20: ChaCha20,
    b3: blake3::Hasher,
}

#[derive(Debug)]
enum SessionError {
    MacMismatch,
}

impl Session {
    fn new(rng: &mut (impl CryptoRng + RngCore)) -> Session {
        let secret = EphemeralSecret::random(rng);
        let pk = secret.public_key();
        Session {
            ready: false,
            secret: secret,
            pk: EncodedPoint::from(pk),
            key: [0; 32],
            cc20: ChaCha20::new_xchacha20(&[0; 32], &[0; 24]),
            b3: blake3::Hasher::new(),
        }
    }

    fn set_sym_key(&mut self, pk: &EncodedPoint) {
        if self.ready {
            panic!("Session already ready");
        }

        let span = info_span!("set_sym_key");
        let _enter = span.enter();

        let pk = PublicKey::from_sec1_bytes(pk.as_ref()).expect("public key is invalid!");
        let shared = self.secret.diffie_hellman(&pk);
        let shared_bytes = shared.raw_secret_bytes();

        self.b3.update(shared_bytes);
        self.key = self.b3.finalize().as_bytes().clone();
        self.b3.reset();
        self.cc20 = ChaCha20::new_xchacha20(&self.key, &[0; 24]);
        debug!("session ready");
        self.ready = true;
    }

    fn encrypt(&mut self, plain: Vec<u8>) -> Vec<u8> {
        if !self.ready {
            panic!("session not ready!")
        };

        let span = info_span!("encrypt");
        let _enter = span.enter();

        let mac = self.mac(&plain);
        debug!("MAC: {}", hex::encode(mac));

        let mut output: Vec<u8> = repeat(0).take(plain.len()).collect();
        self.cc20 = ChaCha20::new_xchacha20(&self.key, &mac);
        self.cc20.process(&plain[..], &mut output[..]);
        output.extend_from_slice(&mac);
        debug!("done");
        output
    }

    fn decrypt(&mut self, mut ciphertext: Vec<u8>) -> Result<Vec<u8>, SessionError> {
        if !self.ready {
            panic!("session not ready!")
        };

        let span = info_span!("decrypt");
        let _enter = span.enter();

        let claimed_mac: Vec<u8> = ciphertext.split_off(ciphertext.len() - 24);
        let mut output: Vec<u8> = repeat(0).take(ciphertext.len()).collect();
        self.cc20 = ChaCha20::new_xchacha20(&self.key, &claimed_mac);
        self.cc20.process(&ciphertext[..], &mut output[..]);

        let calculated_mac = self.mac(&output);
        if claimed_mac != calculated_mac {
            debug!("Claimed MAC: {}", hex::encode(claimed_mac));
            debug!("Calculated MAC: {}", hex::encode(calculated_mac));
            return Err(SessionError::MacMismatch);
        }
        debug!("done");
        Ok(output)
    }

    fn mac(&mut self, plain: &[u8]) -> [u8; 24] {
        if !self.ready {
            panic!("session not ready!")
        };

        self.b3.update(plain);
        self.b3.update(&self.key);

        let mut mac = [0u8; 24];
        self.b3.finalize_xof().fill(&mut mac);
        self.b3.reset();

        mac
    }
}

fn main() {
    tracing_subscriber::fmt::init();

    let mut key: [u8; 32] = [0; 32];
    thread_rng().fill(&mut key);

    let msg = "Hello";

    info!("== xc220b3 demo ==");
    info!("Message: {:?}", msg);

    let mut rng = thread_rng();
    let mut sesh1 = Session::new(&mut rng);
    let mut sesh2 = Session::new(&mut rng);

    // give each session the other's secp256k1 public key so they can derive a
    // shared secret, which is hashed to get the symmetric key (technically ECDHE)

    debug!("sesh1 pk: {}", sesh1.pk);
    debug!("sesh2 pk: {}", sesh2.pk);

    sesh1.set_sym_key(&sesh2.pk);
    sesh2.set_sym_key(&sesh1.pk);

    // when this happens in production, we're using a variation of certificates
    // to exchange the public keys between live signers and valera's server.
    // live signers are given a certificate that contains their public key
    // and a signature of that from valera. the live signer knows the long-term
    // public keys for valera, so they verify that they're talking to valid peers
    // before we give each session the other's public key.

    let plain = msg.as_bytes().to_vec();

    let encrypted_bytes = sesh1.encrypt(plain.clone());

    match sesh2.decrypt(encrypted_bytes.clone()) {
        Ok(plain) => {
            info!("Decrypted: {:?}", str::from_utf8(&plain[..]).unwrap());
        }
        Err(e) => {
            error!("Error: {:?}", e);
        }
    };

    info!("Now attempting message modification...");

    let mut tampered_bytes = sesh1.encrypt(plain.clone());
    tamper_with(&mut tampered_bytes, 1);

    info!(
        "Tampered Encrypted: {}",
        hex::encode(tampered_bytes.clone())
    );

    match sesh2.decrypt(tampered_bytes) {
        Ok(_) => (),
        Err(e) => match e {
            SessionError::MacMismatch => info!("MAC mismatch! Message was tampered with! (expected)"),
        },
    };
}

fn tamper_with(bytes: &mut Vec<u8>, many_times: usize) {
    let mut rng = thread_rng();
    let mut index;

    for _ in 0..many_times {
        index = rng.gen_range(0..bytes.len());
        bytes[index] = rng.gen();
    }
}
