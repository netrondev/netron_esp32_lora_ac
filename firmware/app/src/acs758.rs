//! ACS758 Hall-effect current sensor driver
//!
//! Based on the ACS712 library by Rob Tillaart
//! Adapted for Rust/ESP32 with embedded-hal ADC
//!
//! Supports ACS758 variants:
//! - ACS758LCB-050B: ±50A, 40mV/A (bidirectional)
//! - ACS758LCB-100B: ±100A, 20mV/A (bidirectional)
//! - ACS758KCB-150B: ±150A, 13.3mV/A (bidirectional)
//! - ACS758ECB-200B: ±200A, 10mV/A (bidirectional)

use esp_hal::analog::adc::{Adc, AdcChannel, AdcPin};
use esp_hal::gpio::AnalogPin;
use esp_hal::time::{Duration, Instant};
use esp_hal::Blocking;
use nb::block;

/// Form factor for sinusoidal waveforms (1/sqrt(2))
pub const FORM_FACTOR_SINUS: f32 = 0.707107;

/// Default noise level in mV (from datasheet)
pub const DEFAULT_NOISE_MV: f32 = 21.0;

/// ACS758 sensor variant configurations
#[derive(Clone, Copy)]
pub enum Acs758Variant {
    /// ACS758LCB-050B: ±50A, 40mV/A
    Lcb050B,
    /// ACS758LCB-100B: ±100A, 20mV/A
    Lcb100B,
    /// ACS758KCB-150B: ±150A, 13.3mV/A
    Kcb150B,
    /// ACS758ECB-200B: ±200A, 10mV/A
    Ecb200B,
    /// Custom sensitivity (mV per Amp)
    Custom(f32),
}

impl Acs758Variant {
    /// Get sensitivity in mV/A
    pub fn mv_per_amp(&self) -> f32 {
        match self {
            Acs758Variant::Lcb050B => 40.0,
            Acs758Variant::Lcb100B => 20.0,
            Acs758Variant::Kcb150B => 13.3,
            Acs758Variant::Ecb200B => 10.0,
            Acs758Variant::Custom(mv) => *mv,
        }
    }
}

/// ACS758 current sensor driver
pub struct Acs758<'a, ADCI, P>
where
    P: AdcChannel + AnalogPin,
{
    adc: Adc<'a, ADCI, Blocking>,
    pin: AdcPin<P, ADCI>,

    // ADC configuration
    max_adc: u16,
    mv_per_step: f32,
    ma_per_step: f32,

    // Sensor configuration
    mv_per_ampere: f32,
    form_factor: f32,
    noise_mv: f32,
    midpoint: i32,

    // Options
    suppress_noise: bool,
    micros_adjust: f32,
}

/// Measurement result with debug info
#[derive(Clone, Copy, Default)]
pub struct AcMeasurement {
    /// RMS current in milliamps
    pub ma_rms: f32,
    /// Peak-to-peak current in milliamps
    pub ma_peak2peak: f32,
    /// Detected or configured frequency in Hz
    pub frequency: f32,
    /// Number of samples taken
    pub samples: u32,
    /// Minimum ADC value seen
    pub adc_min: u16,
    /// Maximum ADC value seen
    pub adc_max: u16,
}

impl<'a, ADCI, P> Acs758<'a, ADCI, P>
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
    /// * `vref` - ADC reference voltage (e.g., 3.3V for ESP32 with 11dB attenuation)
    /// * `max_adc` - Maximum ADC value (4095 for 12-bit)
    pub fn new(
        adc: Adc<'a, ADCI, Blocking>,
        pin: AdcPin<P, ADCI>,
        variant: Acs758Variant,
        vref: f32,
        max_adc: u16,
    ) -> Self {
        let mv_per_ampere = variant.mv_per_amp();
        let mv_per_step = 1000.0 * vref / max_adc as f32;
        let ma_per_step = 1000.0 * mv_per_step / mv_per_ampere;

        Self {
            adc,
            pin,
            max_adc,
            mv_per_step,
            ma_per_step,
            mv_per_ampere,
            form_factor: FORM_FACTOR_SINUS,
            noise_mv: DEFAULT_NOISE_MV,
            midpoint: max_adc as i32 / 2,
            suppress_noise: false,
            micros_adjust: 1.0,
        }
    }

    // ========== CALIBRATION: MIDPOINT ==========

    /// Set the midpoint (zero-current ADC value)
    pub fn set_midpoint(&mut self, midpoint: u16) {
        if midpoint <= self.max_adc {
            self.midpoint = midpoint as i32;
        }
    }

    /// Get the current midpoint value
    pub fn midpoint(&self) -> u16 {
        self.midpoint as u16
    }

    /// Increment midpoint by 1
    pub fn inc_midpoint(&mut self) -> u16 {
        if self.midpoint < self.max_adc as i32 {
            self.midpoint += 1;
        }
        self.midpoint as u16
    }

    /// Decrement midpoint by 1
    pub fn dec_midpoint(&mut self) -> u16 {
        if self.midpoint > 0 {
            self.midpoint -= 1;
        }
        self.midpoint as u16
    }

    /// Reset midpoint to default (ADC_MAX / 2)
    pub fn reset_midpoint(&mut self) -> u16 {
        self.midpoint = self.max_adc as i32 / 2;
        self.midpoint as u16
    }

    /// Auto-calibrate midpoint by sampling AC signal
    /// Should be called with no load (zero current) or will average the AC waveform
    pub fn auto_midpoint(&mut self, frequency: f32, cycles: u16) -> u16 {
        let two_periods_us = (2_000_000.0 / frequency) as u64;
        let cycles = if cycles == 0 { 1 } else { cycles };

        let mut total: u32 = 0;

        for _ in 0..cycles {
            let mut sub_total: u32 = 0;
            let mut samples: u32 = 0;

            let start = Instant::now();
            while start.elapsed() < Duration::from_micros(two_periods_us) {
                let reading = self.analog_read();
                sub_total += reading as u32;
                samples += 1;
                delay_us(1);
            }

            if samples > 0 {
                total += sub_total / samples;
            }
        }

        self.midpoint = ((total + cycles as u32 / 2) / cycles as u32) as i32;
        self.midpoint as u16
    }

    /// Auto-calibrate midpoint for DC (simple averaging)
    pub fn auto_midpoint_dc(&mut self, cycles: u16) -> u16 {
        let cycles = if cycles == 0 { 1 } else { cycles };
        let mut total: u32 = 0;

        for _ in 0..cycles {
            total += self.analog_read() as u32;
        }

        self.midpoint = ((total + cycles as u32 / 2) / cycles as u32) as i32;
        self.midpoint as u16
    }

    // ========== CALIBRATION: FORM FACTOR ==========

    /// Set form factor for RMS calculation (default: 0.707 for sine wave)
    pub fn set_form_factor(&mut self, form_factor: f32) {
        self.form_factor = form_factor;
    }

    /// Get current form factor
    pub fn form_factor(&self) -> f32 {
        self.form_factor
    }

    // ========== CALIBRATION: NOISE ==========

    /// Set noise level in mV
    pub fn set_noise_mv(&mut self, noise_mv: f32) {
        self.noise_mv = noise_mv;
    }

    /// Get noise level in mV
    pub fn noise_mv(&self) -> f32 {
        self.noise_mv
    }

    /// Enable/disable noise suppression (averages 2 samples)
    pub fn suppress_noise(&mut self, enable: bool) {
        self.suppress_noise = enable;
    }

    /// Measure noise level by reading peak-to-peak and dividing by 2
    pub fn measure_noise_mv(&mut self, frequency: f32, cycles: u16) -> f32 {
        let ma = self.ma_peak2peak(frequency, cycles);
        ma * self.mv_per_ampere * 0.001 / 2.0
    }

    // ========== CALIBRATION: SENSITIVITY ==========

    /// Set sensitivity in mV/A
    pub fn set_mv_per_amp(&mut self, mv_per_amp: f32) {
        self.mv_per_ampere = mv_per_amp;
        self.ma_per_step = 1000.0 * self.mv_per_step / self.mv_per_ampere;
    }

    /// Get sensitivity in mV/A
    pub fn mv_per_amp(&self) -> f32 {
        self.mv_per_ampere
    }

    /// Get conversion factor in mA per ADC step
    pub fn ma_per_step(&self) -> f32 {
        self.ma_per_step
    }

    // ========== MEASUREMENTS ==========

    /// Measure peak-to-peak current in mA
    pub fn ma_peak2peak(&mut self, frequency: f32, cycles: u16) -> f32 {
        let period_us = (1_000_000.0 / frequency) as u64;
        let cycles = if cycles == 0 { 1 } else { cycles };

        let mut sum: f32 = 0.0;

        for _ in 0..cycles {
            let initial = self.read_sample();
            let mut minimum = initial;
            let mut maximum = initial;

            let start = Instant::now();
            while start.elapsed() < Duration::from_micros(period_us) {
                let value = self.read_sample();
                if value < minimum {
                    minimum = value;
                } else if value > maximum {
                    maximum = value;
                }
            }

            sum += (maximum - minimum) as f32;
        }

        let peak2peak = sum * self.ma_per_step;
        if cycles > 1 {
            peak2peak / cycles as f32
        } else {
            peak2peak
        }
    }

    /// Measure AC current using peak detection with adaptive form factor
    pub fn ma_ac(&mut self, frequency: f32, cycles: u16) -> f32 {
        let period_us = (1_000_000.0 / frequency) as u64;
        let cycles = if cycles == 0 { 1 } else { cycles };

        let zero_level = (self.noise_mv / self.mv_per_step) as i32;
        let mut sum: f32 = 0.0;

        for _ in 0..cycles {
            let mut samples: u32 = 0;
            let mut zeros: u32 = 0;

            let initial = self.read_sample();
            let mut minimum = initial;
            let mut maximum = initial;

            let start = Instant::now();
            while start.elapsed() < Duration::from_micros(period_us) {
                samples += 1;
                let value = self.read_sample();

                if value < minimum {
                    minimum = value;
                } else if value > maximum {
                    maximum = value;
                }

                // Count samples near zero (within noise level)
                let centered = value - self.midpoint;
                if centered.abs() <= zero_level {
                    zeros += 1;
                }
            }

            let peak2peak = maximum - minimum;

            // Adaptive form factor based on zero-crossing duty cycle
            let ff = if samples > 0 && zeros > samples / 40 {
                // More than 2.5% zeros - adjust form factor
                let d = 1.0 - (zeros as f32 / samples as f32);
                libm::sqrtf(d) * self.form_factor
            } else {
                self.form_factor
            };

            sum += peak2peak as f32 * ff;
        }

        let ma = 0.5 * sum * self.ma_per_step;
        if cycles > 1 {
            ma / cycles as f32
        } else {
            ma
        }
    }

    /// Measure AC current using true RMS sampling
    /// This is the most accurate method for non-sinusoidal waveforms
    pub fn ma_ac_sampling(&mut self, frequency: f32, cycles: u16) -> f32 {
        let period_us = (1_000_000.0 / frequency) as u64;
        let cycles = if cycles == 0 { 1 } else { cycles };

        let mut sum: f32 = 0.0;

        for _ in 0..cycles {
            let mut samples: u32 = 0;
            let mut sum_squared: f32 = 0.0;

            let start = Instant::now();
            while start.elapsed() < Duration::from_micros(period_us) {
                samples += 1;
                let value = self.read_sample();
                let current = (value - self.midpoint) as f32;
                sum_squared += current * current;
            }

            if samples > 0 {
                sum += libm::sqrtf(sum_squared / samples as f32);
            }
        }

        let ma = sum * self.ma_per_step;
        if cycles > 1 {
            ma / cycles as f32
        } else {
            ma
        }
    }

    /// Measure AC current with detailed results
    pub fn measure_ac(&mut self, frequency: f32, cycles: u16) -> AcMeasurement {
        let period_us = (1_000_000.0 / frequency) as u64;
        let cycles = if cycles == 0 { 1 } else { cycles };

        let mut rms_sum: f32 = 0.0;
        let mut p2p_sum: f32 = 0.0;
        let mut total_samples: u32 = 0;
        let mut global_min: u16 = u16::MAX;
        let mut global_max: u16 = 0;

        for _ in 0..cycles {
            let mut samples: u32 = 0;
            let mut sum_squared: f32 = 0.0;

            let initial = self.analog_read();
            let mut minimum = initial;
            let mut maximum = initial;

            let start = Instant::now();
            while start.elapsed() < Duration::from_micros(period_us) {
                samples += 1;
                let raw = self.analog_read();

                // Apply noise suppression if enabled
                let value = if self.suppress_noise {
                    let raw2 = self.analog_read();
                    ((raw as u32 + raw2 as u32) / 2) as u16
                } else {
                    raw
                };

                if value < minimum {
                    minimum = value;
                }
                if value > maximum {
                    maximum = value;
                }
                if value < global_min {
                    global_min = value;
                }
                if value > global_max {
                    global_max = value;
                }

                let current = (value as i32 - self.midpoint) as f32;
                sum_squared += current * current;
            }

            if samples > 0 {
                rms_sum += libm::sqrtf(sum_squared / samples as f32);
                total_samples += samples;
            }
            p2p_sum += (maximum - minimum) as f32;
        }

        AcMeasurement {
            ma_rms: rms_sum * self.ma_per_step / cycles as f32,
            ma_peak2peak: p2p_sum * self.ma_per_step / cycles as f32,
            frequency,
            samples: total_samples,
            adc_min: global_min,
            adc_max: global_max,
        }
    }

    /// Measure DC current in mA
    pub fn ma_dc(&mut self, cycles: u16) -> f32 {
        // Read once to stabilize ADC
        let _ = self.analog_read();

        let cycles = if cycles == 0 { 1 } else { cycles };
        let mut sum: i32 = 0;

        for _ in 0..cycles {
            let value = self.read_sample();
            sum += value - self.midpoint;
        }

        let ma = sum as f32 * self.ma_per_step;
        if cycles > 1 {
            ma / cycles as f32
        } else {
            ma
        }
    }

    /// Measure AC current with averaging over multiple measurement cycles
    /// Similar to the example usage: average += ACS.mA_AC_sampling(frequency, 1)
    pub fn ma_ac_averaged(&mut self, frequency: f32, measurement_cycles: u16, averages: u16) -> f32 {
        let averages = if averages == 0 { 1 } else { averages };
        let mut sum: f32 = 0.0;

        for _ in 0..averages {
            sum += self.ma_ac_sampling(frequency, measurement_cycles);
        }

        sum / averages as f32
    }

    // ========== FREQUENCY DETECTION ==========

    /// Detect AC frequency by measuring zero crossings
    /// `min_frequency` sets the timeout for detection
    pub fn detect_frequency(&mut self, min_frequency: f32) -> f32 {
        // First pass: find min/max
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
            while self.analog_read() > q1 && start.elapsed() < Duration::from_micros(timeout_us_10x)
            {
            }
            while self.analog_read() <= q3 && start.elapsed() < Duration::from_micros(timeout_us_10x)
            {
            }
        }
        let elapsed = measure_start.elapsed().as_micros();

        // Calculate frequency (10 cycles measured)
        let wavelength_us = elapsed as f32;
        let frequency = 10_000_000.0 / wavelength_us;

        frequency * self.micros_adjust
    }

    /// Set timing adjustment factor for frequency detection
    pub fn set_micros_adjust(&mut self, factor: f32) {
        self.micros_adjust = factor;
    }

    /// Get timing adjustment factor
    pub fn micros_adjust(&self) -> f32 {
        self.micros_adjust
    }

    // ========== DEBUG ==========

    /// Get minimum ADC value over a time period
    pub fn get_minimum(&mut self, millis: u32) -> u16 {
        let mut minimum = self.analog_read();
        let start = Instant::now();

        while start.elapsed() < Duration::from_millis(millis as u64) {
            let value = self.analog_read();
            if value < minimum {
                minimum = value;
            }
        }

        minimum
    }

    /// Get maximum ADC value over a time period
    pub fn get_maximum(&mut self, millis: u32) -> u16 {
        let mut maximum = self.analog_read();
        let start = Instant::now();

        while start.elapsed() < Duration::from_millis(millis as u64) {
            let value = self.analog_read();
            if value > maximum {
                maximum = value;
            }
        }

        maximum
    }

    /// Read raw ADC value
    pub fn analog_read(&mut self) -> u16 {
        block!(self.adc.read_oneshot(&mut self.pin)).unwrap_or(0)
    }

    // ========== PRIVATE ==========

    /// Read a sample, optionally with noise suppression
    fn read_sample(&mut self) -> i32 {
        let value = self.analog_read();
        if self.suppress_noise {
            let value2 = self.analog_read();
            ((value as u32 + value2 as u32) / 2) as i32
        } else {
            value as i32
        }
    }
}

fn delay_us(us: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_micros(us) {}
}
