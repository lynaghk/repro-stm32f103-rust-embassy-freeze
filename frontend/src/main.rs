use schema::Command;

const MAX_PACKET_SIZE: usize = 64;

fn main() {
    let di = nusb::list_devices()
        .unwrap()
        .find(|d| d.vendor_id() == 0xc0de && d.product_id() == 0xcafe)
        .expect("device should be connected");

    eprintln!("Device info: {di:?}");

    let device = di.open().unwrap();

    let interface = device.claim_interface(0).unwrap();

    let endpoint_addr = 1;
    let mut in_queue = interface.bulk_in_queue(0x80 + endpoint_addr);
    let mut out_queue = interface.bulk_out_queue(endpoint_addr);
    let mut i = 0;
    loop {
        ////////////////////////////////
        // Read and print ADC values

        while in_queue.pending() < 1 {
            in_queue.submit(nusb::transfer::RequestBuffer::new(MAX_PACKET_SIZE));
        }

        let completion = futures_lite::future::block_on(in_queue.next_complete());

        let data = completion.data.as_slice();
        for chunk in data.chunks_exact(2) {
            if let [low, high] = chunk {
                let adc_value = u16::from_le_bytes([*low, *high]);
                println!("{}", adc_value);
            }
        }
        in_queue.submit(nusb::transfer::RequestBuffer::reuse(
            completion.data,
            MAX_PACKET_SIZE,
        ));

        ///////////////////////////////////////
        // Send a command down to the device

        // let mut buf = [0u8; MAX_PACKET_SIZE];
        // let command = Command::SetFrequency {
        //     frequency_kHz: ((i % 100) + 1) as f64,
        // };

        // if let Ok(serialized) = command.serialize(&mut buf) {
        //     out_queue.submit(serialized.into());
        // }
        // i += 1;
    }
}
