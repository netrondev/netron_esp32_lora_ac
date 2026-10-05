//! AC Current measurement module using ACS758LCB-050B sensor
//!
//! Uses the ACS758 driver with averaging for accurate RMS measurement

use crate::acs758::{Acs758, Acs758Variant};
use esp_hal::analog::adc::{Adc, AdcChannel, AdcConfig, Attenuation};
use esp_hal::gpio::AnalogPin;
use esp_hal::time::{Duration, Instant};

// ADC configuration
const ADC_MAX: u16 = 4095; // 12-bit ADC
const ADC_VREF: f32 = 3.3; // ADC reference voltage with 11dB attenuation

// Measurement configuration
const AC_FREQUENCY: f32 = 50.0; // Mains frequency (Hz)
const MEASUREMENT_AVERAGES: u16 = 100; // Number of measurements to average
const CYCLES_PER_MEASUREMENT: u16 = 1; // AC cycles per individual measurement

// Waveform capture parameters (for visualization)
// 500 samples at 25kHz = 20ms = 1 cycle of 50Hz
pub const WAVEFORM_SAMPLES: usize = 500;
const WAVEFORM_INTERVAL_US: u64 = 40; // 40μs = 25kHz

pub struct CurrentSensor<'a, ADCI, P>
where
    P: AdcChannel + AnalogPin,
{
    acs: Acs758<'a, ADCI, P>,
    configured_voltage: f32,
    frequency: f32,
}

#[derive(Clone, Copy)]
pub struct PowerReading {
    pub current_rms: f32,    // Amps RMS
    pub voltage: f32,        // Configured voltage (V)
    pub power_apparent: f32, // VA (Volt-Amps)
    pub power_real: f32,     // Watts (assuming PF=1 for resistive loads)
    pub power_factor: f32,   // Power factor (1.0 for now)
    pub frequency: f32,      // Configured/detected frequency (Hz)
    // Debug values
    pub adc_min: u16,     // Min raw ADC value in sample window
    pub adc_max: u16,     // Max raw ADC value in sample window
    pub adc_rms_raw: f32, // RMS in ADC counts (before conversion)
}

impl<'a, ADCI, P> CurrentSensor<'a, ADCI, P>
where
    P: AdcChannel + AnalogPin,
    ADCI: esp_hal::analog::adc::RegisterAccess + 'a,
{
    pub fn new(adc_peripheral: ADCI, pin: P, voltage: f32) -> Self {
        let mut adc_config = AdcConfig::new();
        let adc_pin = adc_config.enable_pin(pin, Attenuation::_11dB);
        let adc = Adc::new(adc_peripheral, adc_config);

        let acs = Acs758::new(
            adc,
            adc_pin,
            Acs758Variant::Lcb050B, // 40mV/A
            ADC_VREF,
            ADC_MAX,
        );

        Self {
            acs,
            configured_voltage: voltage,
            frequency: AC_FREQUENCY,
        }
    }

    pub fn set_voltage(&mut self, voltage: f32) {
        self.configured_voltage = voltage;
    }

    pub fn set_frequency(&mut self, frequency: f32) {
        self.frequency = frequency;
    }

    /// Auto-calibrate the midpoint with no load connected
    pub fn calibrate(&mut self) {
        self.acs.auto_midpoint(self.frequency, 10);
    }

    /// Get the current midpoint value
    pub fn midpoint(&self) -> u16 {
        self.acs.midpoint()
    }

    /// Set the midpoint manually
    pub fn set_midpoint(&mut self, midpoint: u16) {
        self.acs.set_midpoint(midpoint);
    }

    pub fn read(&mut self) -> PowerReading {
        // Averaging loop similar to the Arduino example:
        // float average = 0;
        // for (int i = 0; i < 100; i++) {
        //     average += ACS.mA_AC_sampling(frequency, 1);
        // }
        // float mA = average / 100.0;

        let mut sum_ma: f32 = 0.0;
        let mut min_raw: u16 = u16::MAX;
        let mut max_raw: u16 = 0;

        for _ in 0..MEASUREMENT_AVERAGES {
            // Get detailed measurement which includes min/max
            let measurement = self.acs.measure_ac(self.frequency, CYCLES_PER_MEASUREMENT);

            sum_ma += measurement.ma_rms;

            if measurement.adc_min < min_raw {
                min_raw = measurement.adc_min;
            }
            if measurement.adc_max > max_raw {
                max_raw = measurement.adc_max;
            }
        }

        let ma_rms = sum_ma / MEASUREMENT_AVERAGES as f32;
        let current_rms = ma_rms / 1000.0; // Convert mA to A

        // Calculate ADC RMS (for debug/noise floor detection)
        // This is the amplitude in ADC counts
        let adc_amplitude = (max_raw as f32 - min_raw as f32) / 2.0;
        let adc_rms_raw = adc_amplitude * 0.707; // RMS of sine wave

        // Calculate power values
        let power_apparent = current_rms * self.configured_voltage;
        let power_factor = 1.0; // Assume resistive load
        let power_real = power_apparent * power_factor;

        PowerReading {
            current_rms,
            voltage: self.configured_voltage,
            power_apparent,
            power_real,
            power_factor,
            frequency: self.frequency,
            adc_min: min_raw,
            adc_max: max_raw,
            adc_rms_raw,
        }
    }

    /// Read with custom averaging count
    pub fn read_averaged(&mut self, averages: u16) -> PowerReading {
        let averages = if averages == 0 { 1 } else { averages };

        let mut sum_ma: f32 = 0.0;
        let mut min_raw: u16 = u16::MAX;
        let mut max_raw: u16 = 0;

        for _ in 0..averages {
            let measurement = self.acs.measure_ac(self.frequency, CYCLES_PER_MEASUREMENT);

            sum_ma += measurement.ma_rms;

            if measurement.adc_min < min_raw {
                min_raw = measurement.adc_min;
            }
            if measurement.adc_max > max_raw {
                max_raw = measurement.adc_max;
            }
        }

        let ma_rms = sum_ma / averages as f32;
        let current_rms = ma_rms / 1000.0;

        let adc_amplitude = (max_raw as f32 - min_raw as f32) / 2.0;
        let adc_rms_raw = adc_amplitude * 0.707;

        let power_apparent = current_rms * self.configured_voltage;
        let power_factor = 1.0;
        let power_real = power_apparent * power_factor;

        PowerReading {
            current_rms,
            voltage: self.configured_voltage,
            power_apparent,
            power_real,
            power_factor,
            frequency: self.frequency,
            adc_min: min_raw,
            adc_max: max_raw,
            adc_rms_raw,
        }
    }

    /// Capture raw ADC waveform for visualization
    /// Returns 500 samples at 25kHz (20ms total = 1 cycle of 50Hz)
    pub fn capture_waveform(&mut self) -> [u16; WAVEFORM_SAMPLES] {
        let mut waveform = [0u16; WAVEFORM_SAMPLES];

        for i in 0..WAVEFORM_SAMPLES {
            waveform[i] = self.acs.analog_read();
            delay_us(WAVEFORM_INTERVAL_US);
        }

        waveform
    }

    /// Get direct access to the ACS758 driver for advanced usage
    pub fn acs(&mut self) -> &mut Acs758<'a, ADCI, P> {
        &mut self.acs
    }
}

fn delay_us(us: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_micros(us) {}
}
