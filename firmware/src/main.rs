#![no_std]
#![no_main]

use schema::*;

use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_stm32::adc::Adc;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::time::Hertz;
use embassy_stm32::{adc, bind_interrupts, peripherals, usb, Config};
use embassy_time::Timer;
use embassy_usb::driver::{Endpoint, EndpointIn, EndpointOut};
use embassy_usb::Builder;

use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USB_LP_CAN1_RX0 => usb::InterruptHandler<peripherals::USB>;
});

const MAX_PACKET_SIZE: u8 = 64;
const SAMPLES_PER_PACKET: usize = (MAX_PACKET_SIZE as usize) / 2; // 2 bytes per sample
pub const USB_CLASS_CUSTOM: u8 = 0xFF;
const USB_SUBCLASS_CUSTOM: u8 = 0x00;
const USB_PROTOCOL_CUSTOM: u8 = 0x00;

// I believe this is the default, but I'm adding explicitly just in case.
// In any case, we never make it here =(
use cortex_m_rt::{exception, ExceptionFrame};
#[exception]
unsafe fn HardFault(_ef: &ExceptionFrame) -> ! {
    loop {}
}

static mut DMA_TARGET: u32 = 0;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut config = Config::default();
    {
        use embassy_stm32::rcc::*;
        config.rcc.hse = Some(Hse {
            freq: Hertz(8_000_000),
            mode: HseMode::Oscillator,
        });
        config.rcc.pll = Some(Pll {
            src: PllSource::HSE,
            prediv: PllPreDiv::DIV1,
            mul: PllMul::MUL9,
        });
        config.rcc.sys = Sysclk::PLL1_P;
        config.rcc.ahb_pre = AHBPrescaler::DIV1;
        config.rcc.apb1_pre = APBPrescaler::DIV2;
        config.rcc.apb2_pre = APBPrescaler::DIV1;
    }
    let mut p = embassy_stm32::init(config);

    info!("Hello World!");

    {
        // Board has a pull-up resistor on the D+ line; pull it down to send a RESET condition to the USB bus.
        // This forced reset is needed only for development, without it host will not reset your device when you upload new firmware.
        let _dp = Output::new(&mut p.PA12, Level::Low, Speed::Low);
        Timer::after_millis(10).await;
    }

    ////////////////////////
    // USB Setup

    let driver = embassy_stm32::usb::Driver::new(p.USB, Irqs, p.PA12, p.PA11);
    let (vid, pid) = (0xc0de, 0xcafe);
    let mut config = embassy_usb::Config::new(vid, pid);
    config.max_packet_size_0 = MAX_PACKET_SIZE;
    config.product = Some("Calipertron");

    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut [], // no msos descriptors
        &mut control_buf,
    );

    let mut func = builder.function(USB_CLASS_CUSTOM, USB_SUBCLASS_CUSTOM, USB_PROTOCOL_CUSTOM);
    let mut iface = func.interface();

    let mut iface_alt = iface.alt_setting(
        USB_CLASS_CUSTOM,
        USB_SUBCLASS_CUSTOM,
        USB_PROTOCOL_CUSTOM,
        None,
    );
    let mut read_ep = iface_alt.endpoint_bulk_out(MAX_PACKET_SIZE as u16);
    let mut write_ep = iface_alt.endpoint_bulk_in(MAX_PACKET_SIZE as u16);
    drop(func);

    let mut usb = builder.build();
    let fut_usb = usb.run();

    ////////////////////////
    // Timer-driven DMA setup

    let tim = embassy_stm32::timer::low_level::Timer::new(p.TIM1);
    let timer_registers = tim.regs_advanced();
    timer_registers
        .cr2()
        .modify(|w| w.set_ccds(embassy_stm32::pac::timer::vals::Ccds::ONUPDATE));

    timer_registers.dier().modify(|w| w.set_ude(true)); // Enable update DMA request
    tim.set_frequency(Hertz(1_000));
    tim.start();

    use embassy_stm32::dma::*;
    let mut opts = TransferOptions::default();
    opts.circular = true;

    // When I originally came across this bug I had DMA writing to GPIO, but the bug persists even when DMA just writes to memory.
    // let dma_target = gpioa.bsrr().as_ptr() as *mut u32;
    let dma_target = unsafe { core::ptr::addr_of!(DMA_TARGET) as *mut u32 };

    let request = embassy_stm32::timer::UpDma::request(&p.DMA1_CH5);

    // comment this out to disable timer-driven DMA
    let _transfer = unsafe { Transfer::new_write(p.DMA1_CH5, request, &[0u32], dma_target, opts) };

    ////////////////////////
    // ADC setup
    // comment out everything below and the future to disable

    let mut adc_buffer = [0; 2 * SAMPLES_PER_PACKET];
    let request = embassy_stm32::adc::RxDma::request(&p.DMA1_CH1);
    let mut opts = TransferOptions::default();
    opts.half_transfer_ir = true;
    let mut adc_rb = unsafe {
        ReadableRingBuffer::new(
            p.DMA1_CH1,
            request,
            embassy_stm32::pac::ADC1.dr().as_ptr() as *mut u16,
            &mut adc_buffer,
            opts,
        )
    };

    // delegate to Embassy to enable clocks and calibrate.
    let mut adc = Adc::new(p.ADC1);

    let vrefint_sample = {
        let mut vrefint = adc.enable_vref();

        // give vref some time to warm up
        Timer::after_millis(100).await;

        adc.read(&mut vrefint).await as u32
    };
    info!("VREFINT: {}", vrefint_sample);

    // Configure ADC for continuous conversion with DMA
    let adc = embassy_stm32::pac::ADC1;

    adc.cr1().modify(|w| {
        w.set_scan(true);
        w.set_eocie(true);
    });

    adc.cr2().modify(|w| {
        w.set_dma(true);
        w.set_cont(true)
    });

    // Configure channel and sampling time
    adc.sqr1().modify(|w| w.set_l(0)); // one conversion.

    const PIN_CHANNEL: u8 = 9; // PB1 is on channel 9 for STM32F103
    adc.sqr3().modify(|w| w.set_sq(0, PIN_CHANNEL));
    adc.smpr2()
        .modify(|w| w.set_smp(PIN_CHANNEL as usize, adc::SampleTime::CYCLES239_5));

    // Start ADC conversions
    adc.cr2().modify(|w| w.set_adon(true));

    ////////////////////////////
    // Data to host future

    let fut_stream_adc = async {
        // Wait for USB to connect
        write_ep.wait_enabled().await;

        // Start handling DMA requests from ADC
        adc_rb.start();
        let mut buf = [0; SAMPLES_PER_PACKET];
        loop {
            loop {
                let r = adc_rb.read_exact(&mut buf).await;

                if r.is_err() {
                    error!("ADC_RB error: {:?}", r);
                    break;
                }

                let r = write_ep.write(bytemuck::cast_slice(&buf)).await;
                if r.is_err() {
                    error!("USB Error: {:?}", r);
                    break;
                }
            }

            adc_rb.clear();
        }
    };

    // Note: ADC is required to trigger bug. Issue disappears if all ADC stuff is commented out and this future which streams an empty buffer is used instead.

    // let fut_stream_adc = async {
    //     // Wait for USB to connect
    //     write_ep.wait_enabled().await;

    //     let buf = [0u16; SAMPLES_PER_PACKET];
    //     loop {
    //         let r = write_ep.write(bytemuck::cast_slice(&buf)).await;
    //         if r.is_err() {
    //             error!("USB Error: {:?}", r);
    //             break;
    //         }
    //     }
    // };

    /////////////////////////////
    // Data from host future
    let fut_commands = async {
        // Wait for USB to connect
        read_ep.wait_enabled().await;

        loop {
            let mut command_buf = [0u8; MAX_PACKET_SIZE as usize];

            match read_ep.read(&mut command_buf).await {
                Ok(size) => {
                    if let Some(command) = Command::deserialize(&command_buf[..size]) {
                        info!("Received command: {:?}", command);
                    } else {
                        error!("Failed to deserialize command");
                    }
                }
                Err(e) => {
                    error!("Failed to read USB packet: {:?}", e);
                }
            };
        }
    };

    // Reading USB from host required to trigger bug, can replace the future above with this commentted version to verify.

    // let fut_commands = async {
    //     // Wait for USB to connect
    //     read_ep.wait_enabled().await;

    //     loop {
    //         Timer::after_secs(1).await;
    //     }
    // };

    let fut_ping = async {
        loop {
            info!("ping");
            Timer::after_secs(1).await;
        }
    };

    join(fut_ping, join(fut_commands, join(fut_usb, fut_stream_adc))).await;
}
