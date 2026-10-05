//! ACS758 Hall-effect current sensor driver - Arduino-style implementation
//!
//! Based directly on the simple-circuit.com Arduino code:
//! - Uses fixed 256 samples with 16x oversampling
//! - RMS calculation: sqrt(sum((sample - offset)^2) / n)
//! - Auto-calibration of DC offset at startup

use esp_hal::analog::adc::{Adc, AdcChannel, AdcPin};
use esp_hal::gpio::AnalogPin;
use esp_hal::Blocking;
use nb::block;

// Configuration constants (matching Arduino)
const N: usize = 256; // Number of samples (same as Arduino)
const OVERSAMPLE: u16 = 16; // 16x oversampling for noise reduction
const OVERSAMPLE_DIV: f32 = 4.0; // Divide by 4 to get 12-bit equivalent

/// Current measurement type
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CurrentType {
    /// AC current - calculates dynamic DC offset from signal average
    Ac,
    /// DC current - uses pre-calibrated DC offset
    Dc,
    /// AC+DC current - AC with DC offset, uses pre-calibrated offset
    AcDc,
}

/// ACS758 sensor variant configurations
#[derive(Clone, Copy)]
pub enum Acs758Variant {
    /// ACS758LCB-050B: ±50A, 40mV/A
    Lcb050B,
    /// ACS758LCB-100B: ±100A, 20mV/A
    Lcb100B,
    /// ACS758KCB-150B: ±150A, 13.3mV/A
    Kcb150B,
    /// ACS758ECB-200B: ±200A, 10mV/A (same as Arduino example)
    Ecb200B,
}

impl Acs758Variant {
    /// Get sensitivity in mV/A
    pub fn mv_per_amp(&self) -> f32 {
        match self {
            Acs758Variant::Lcb050B => 40.0,
            Acs758Variant::Lcb100B => 20.0,
            Acs758Variant::Kcb150B => 13.3,
            Acs758Variant::Ecb200B => 10.0,
        }
    }
}

/// Measurement result
#[derive(Clone, Copy, Default, Debug)]
pub struct Measurement {
    /// Current in Amps
    pub current_amps: f32,
    /// Current type used for measurement
    pub current_type: u8, // 0=AC, 1=DC, 2=AC+DC
    /// Raw ADC voltage (in mV, after conversion)
    pub voltage_mv: f32,
}

/// ACS758 Arduino-style driver
pub struct Acs758Arduino<'a, ADCI, P>
where
    P: AdcChannel + AnalogPin,
{
    adc: Adc<'a, ADCI, Blocking>,
    pin: AdcPin<P, ADCI>,

    // ADC configuration
    adc_max: u16,
    adc_vref_mv: f32, // Reference voltage in millivolts

    // Sensor configuration
    sensitivity: f32, // mV/A

    // Calibration
    dc_offset: f32, // Auto-calibrated DC offset (in ADC units, 12-bit equivalent)

    // Sample array (like Arduino's _array[n])
    samples: [f32; N],
}

impl<'a, ADCI, P> Acs758Arduino<'a, ADCI, P>
where
    P: AdcChannel + AnalogPin,
    ADCI: esp_hal::analog::adc::RegisterAccess + 'a,
{
    /// Create a new ACS758 sensor driver
    ///
    /// # Arguments
    /// * `adc` - ADC instance
    /// * `pin` - ADC pin connected to sensor output
    /// * `variant` - Sensor variant (determines sensitivity)
    /// * `vref_mv` - ADC reference voltage in millivolts (e.g., 3300 for 3.3V)
    /// * `adc_max` - Maximum ADC value (4095 for 12-bit)
    pub fn new(
        adc: Adc<'a, ADCI, Blocking>,
        pin: AdcPin<P, ADCI>,
        variant: Acs758Variant,
        vref_mv: f32,
        adc_max: u16,
    ) -> Self {
        Self {
            adc,
            pin,
            adc_max,
            adc_vref_mv: vref_mv,
            sensitivity: variant.mv_per_amp(),
            dc_offset: adc_max as f32 / 2.0, // Default midpoint
            samples: [0.0; N],
        }
    }

    /// Auto-calibrate DC offset (call with no current flowing through sensor)
    /// This is equivalent to the Arduino setup() calibration
    pub fn calibrate(&mut self) {
        // Get samples
        self.get_samples();

        // Calculate average as DC offset
        let mut sum: f32 = 0.0;
        for i in 0..N {
            sum += self.samples[i];
        }
        self.dc_offset = sum / N as f32;
    }

    /// Get current DC offset value
    pub fn dc_offset(&self) -> f32 {
        self.dc_offset
    }

    /// Set DC offset manually
    pub fn set_dc_offset(&mut self, offset: f32) {
        self.dc_offset = offset;
    }

    /// Analog data acquisition function - matches Arduino get_smaples()
    ///
    /// Fills the sample array with 12-bit equivalent data using 16x oversampling
    fn get_samples(&mut self) {
        // Clear sample array
        for i in 0..N {
            self.samples[i] = 0.0;
        }

        // Ignore the first reading (let ADC stabilize)
        let _ = self.analog_read();

        // Fill samples array with 12-bit data (add another 2 bits using oversampling technique)
        // Arduino: for each sample, sum 16 readings then divide by 4
        for i in 0..N {
            let mut sum: u32 = 0;
            for _ in 0..OVERSAMPLE {
                sum += self.analog_read() as u32;
            }
            // Arduino divides by 4.0 to convert 16x oversampled 10-bit to 12-bit
            // ESP32 already has 12-bit, so we divide by 16 to average, then could
            // potentially get 14-bit, but we'll match Arduino behavior
            self.samples[i] = sum as f32 / OVERSAMPLE_DIV;
        }
    }

    /// Measure current - main measurement function matching Arduino loop()
    pub fn measure(&mut self, current_type: CurrentType) -> Measurement {
        // Get samples from ACS758
        self.get_samples();

        let voltage: f32;

        match current_type {
            CurrentType::Ac => {
                // AC signal: calculate dynamic DC offset from signal average
                let mut offset: f32 = 0.0;
                for i in 0..N {
                    offset += self.samples[i];
                }
                offset /= N as f32;

                // Calculate RMS: sqrt(sum((sample - offset)^2) / n)
                let mut sum_sq: f32 = 0.0;
                for i in 0..N {
                    let diff = self.samples[i] - offset;
                    sum_sq += diff * diff;
                }
                voltage = libm::sqrtf(sum_sq / N as f32);
            }
            CurrentType::AcDc => {
                // AC+DC signal: use pre-calibrated DC offset
                let mut sum_sq: f32 = 0.0;
                for i in 0..N {
                    let diff = self.samples[i] - self.dc_offset;
                    sum_sq += diff * diff;
                }
                voltage = libm::sqrtf(sum_sq / N as f32);
            }
            CurrentType::Dc => {
                // DC signal: average minus pre-calibrated offset
                let mut sum: f32 = 0.0;
                for i in 0..N {
                    sum += self.samples[i] - self.dc_offset;
                }
                voltage = sum / N as f32;
            }
        }

        // Convert ADC value to actual voltage in mV
        // Arduino: acs758_voltage = acs758_voltage * REF_VOLTAGE / bgref_voltage
        // We skip bandgap calibration for now, assume stable Vref
        let voltage_mv = voltage * self.adc_vref_mv / self.adc_max as f32;

        // Calculate current: voltage_mv / sensitivity_mv_per_amp
        let current_amps = voltage_mv / self.sensitivity;

        Measurement {
            current_amps,
            current_type: current_type as u8,
            voltage_mv,
        }
    }

    /// Measure AC current (convenience method)
    pub fn measure_ac(&mut self) -> Measurement {
        self.measure(CurrentType::Ac)
    }

    /// Measure DC current (convenience method)
    pub fn measure_dc(&mut self) -> Measurement {
        self.measure(CurrentType::Dc)
    }

    /// Measure AC+DC current (convenience method)
    pub fn measure_ac_dc(&mut self) -> Measurement {
        self.measure(CurrentType::AcDc)
    }

    /// Measure with averaging (like the example usage pattern)
    /// Takes multiple measurements and averages them
    pub fn measure_averaged(&mut self, current_type: CurrentType, averages: u16) -> Measurement {
        let averages = if averages == 0 { 1 } else { averages };
        let mut sum_amps: f32 = 0.0;
        let mut sum_mv: f32 = 0.0;

        for _ in 0..averages {
            let m = self.measure(current_type);
            sum_amps += m.current_amps;
            sum_mv += m.voltage_mv;
        }

        Measurement {
            current_amps: sum_amps / averages as f32,
            current_type: current_type as u8,
            voltage_mv: sum_mv / averages as f32,
        }
    }

    /// Read raw ADC value
    pub fn analog_read(&mut self) -> u16 {
        block!(self.adc.read_oneshot(&mut self.pin)).unwrap_or(0)
    }

    /// Detect AC frequency by measuring zero crossings
    /// `min_frequency` sets the timeout for detection (e.g., 40.0 for 40Hz minimum)
    /// Returns detected frequency in Hz
    pub fn detect_frequency(&mut self, min_frequency: f32) -> f32 {
        use esp_hal::time::{Duration, Instant};

        // First pass: find min/max over one period
        let timeout_us = (1_000_000.0 / min_frequency) as u64;

        let initial = self.analog_read();
        let mut minimum = initial;
        let mut maximum = initial;

        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(timeout_us) {
            let value = self.analog_read();
            if value > maximum {
                maximum = value;
            }
            if value < minimum {
                minimum = value;
            }
        }

        // Calculate quarter points for more robust detection
        let q1 = ((3 * minimum as u32 + maximum as u32) / 4) as u16;
        let q3 = ((minimum as u32 + 3 * maximum as u32) / 4) as u16;

        // Wait for signal to be below Q1
        let timeout_us_10x = timeout_us * 10;
        let start = Instant::now();
        while self.analog_read() > q1 && start.elapsed() < Duration::from_micros(timeout_us_10x) {}

        // Wait for signal to rise above Q3
        while self.analog_read() <= q3 && start.elapsed() < Duration::from_micros(timeout_us_10x) {}

        // Measure 10 cycles
        let measure_start = Instant::now();
        for _ in 0..10 {
            while self.analog_read() > q1 && start.elapsed() < Duration::from_micros(timeout_us_10x) {}
            while self.analog_read() <= q3 && start.elapsed() < Duration::from_micros(timeout_us_10x) {}
        }
        let elapsed = measure_start.elapsed().as_micros();

        // Calculate frequency (10 cycles measured)
        let wavelength_us = elapsed as f32;
        10_000_000.0 / wavelength_us
    }

    /// Get raw sample array (for debugging)
    pub fn get_sample_array(&self) -> &[f32; N] {
        &self.samples
    }

    /// Get min/max from last sample array (for debugging)
    pub fn get_sample_min_max(&self) -> (f32, f32) {
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for i in 0..N {
            if self.samples[i] < min {
                min = self.samples[i];
            }
            if self.samples[i] > max {
                max = self.samples[i];
            }
        }
        (min, max)
    }
}
