//! Phase 1c DoD: "loopback integration test exchanging messages on every
//! stream type."

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use kvm_protocol::{
    ButtonState, ClipboardContent, ClipboardMessage, ControlMessage, InputMessage, Message,
    MouseButton, TransferMessage,
};

#[tokio::test]
async fn exchanges_a_message_on_every_concern_stream_both_directions() {
    let (a, b) = support::mutually_trusting_pair([1u8; 32], [2u8; 32]);

    let connect_task = {
        let addr = b.addr;
        let our_id = a.device_id;
        let endpoint = a.endpoint.clone();
        tokio::spawn(async move { kvm_net::connect(&endpoint, addr, our_id).await })
    };
    let accept_task = {
        let our_id = b.device_id;
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("endpoint closed");
            kvm_net::accept(incoming, our_id).await
        })
    };

    let mut peer_a = connect_task.await.unwrap().unwrap();
    let mut peer_b = accept_task.await.unwrap().unwrap();

    assert_eq!(peer_a.remote_device_id, b.device_id);
    assert_eq!(peer_b.remote_device_id, a.device_id);

    // Control
    let ping = Message::Control(ControlMessage::Ping { nonce: 7 });
    peer_a.streams.control.send(&ping).await.unwrap();
    assert_eq!(peer_b.streams.control.recv().await.unwrap(), ping);

    // Input
    let key = Message::Input(InputMessage::Key {
        keycode: 65,
        state: ButtonState::Pressed,
    });
    peer_a.streams.input.send(&key).await.unwrap();
    assert_eq!(peer_b.streams.input.recv().await.unwrap(), key);

    let mouse = Message::Input(InputMessage::MouseButton {
        button: MouseButton::Left,
        state: ButtonState::Released,
    });
    peer_b.streams.input.send(&mouse).await.unwrap();
    assert_eq!(peer_a.streams.input.recv().await.unwrap(), mouse);

    // Clipboard
    let clip = Message::Clipboard(ClipboardMessage::Update {
        content: ClipboardContent::Text("hello from A".to_string()),
    });
    peer_a.streams.clipboard.send(&clip).await.unwrap();
    assert_eq!(peer_b.streams.clipboard.recv().await.unwrap(), clip);

    // Transfer
    let offer = Message::Transfer(TransferMessage::Offer {
        id: 1,
        file_name: "photo.png".to_string(),
        size: 4096,
    });
    peer_a.streams.transfer.send(&offer).await.unwrap();
    assert_eq!(peer_b.streams.transfer.recv().await.unwrap(), offer);
}
