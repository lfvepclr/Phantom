use divan::Bencher;
use phantom_core::crypto::cipher::CipherSuite;
use phantom_core::crypto::session::CipherOffer;
use phantom_core::crypto::{KeyPair, NoiseInitiator, NoiseResponder, Psk};
use tokio::io::duplex;

#[divan::bench]
fn noise_ikpsk1_handshake(bencher: Bencher) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    bencher.bench_local(|| {
        rt.block_on(async {
            let server_kp = KeyPair::generate().unwrap();
            let client_kp = KeyPair::generate().unwrap();
            let psk = Psk::generate();
            let (client_stream, server_stream) = duplex(65536);

            let server_secret = server_kp.secret;
            let client_secret = client_kp.secret;
            let server_public = server_kp.public;

            let offer = CipherOffer::default_offer();
            let supported: Vec<CipherSuite> = CipherSuite::all_ordered().to_vec();

            let server_psk = psk.clone();
            let server_handle = tokio::spawn(async move {
                NoiseResponder::new(&server_secret, server_psk)
                    .handshake(server_stream, &supported)
                    .await
                    .unwrap()
            });

            let client_psk = psk.clone();
            let client_handle = tokio::spawn(async move {
                NoiseInitiator::new(&client_secret, &server_public, client_psk)
                    .handshake(client_stream, &offer)
                    .await
                    .unwrap()
            });

            let _ = (server_handle.await.unwrap(), client_handle.await.unwrap());
        });
    });
}

fn main() {
    divan::main();
}
