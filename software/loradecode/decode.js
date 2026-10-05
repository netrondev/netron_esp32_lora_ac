/**
 * ESP32 power monitor payload decoder.
 *
 * Mirrors software/milesight_d4/src/tlv.rs — see docs/PROTOCOL.md. Records are
 * channel(1) | type(1) | value(N), little-endian, with no length byte on the
 * wire: each channel/type pair has an implied length, so an unknown pair means
 * the rest of the frame cannot be located and decoding stops there.
 *
 * Written in ES5 with no dependencies so it runs unchanged in ChirpStack, TTN
 * and Node-RED sandboxes.
 *
 * Standalone use:
 *   node decode.js <base64|hex>
 */

/* eslint-disable */
// ChirpStack v4
function decodeUplink(input) {
    return { data: decodePayload(input.bytes) };
}

// ChirpStack v3
function Decode(fPort, bytes) {
    return decodePayload(bytes);
}

// The Things Network
function Decoder(bytes, port) {
    return decodePayload(bytes);
}
/* eslint-enable */

function decodePayload(bytes) {
    var decoded = {};

    for (var i = 0; i < bytes.length; ) {
        if (i + 1 >= bytes.length) {
            decoded.undecoded_at = i;
            break;
        }

        var channel = bytes[i];
        var type = bytes[i + 1];
        var value = i + 2;
        var consumed = -1;

        // ---- Device information, sent after each join ----
        if (channel === 0xff && type === 0x0b) {
            decoded.power_on = true;
            consumed = 1;
        } else if (channel === 0xff && type === 0x01) {
            decoded.protocol_version = bytes[value];
            consumed = 1;
        } else if (channel === 0xff && type === 0x09) {
            decoded.hardware_version = "v" + bytes[value] + "." + bytes[value + 1];
            consumed = 2;
        } else if (channel === 0xff && type === 0x0a) {
            decoded.firmware_version = "v" + bytes[value] + "." + bytes[value + 1];
            consumed = 2;
        } else if (channel === 0xff && type === 0x0f) {
            decoded.lorawan_class = readClass(bytes[value]);
            consumed = 1;
        } else if (channel === 0xff && type === 0x16) {
            decoded.sn = readHex(bytes, value, 8);
            consumed = 8;

        // ---- Measurements ----
        } else if (channel === 0x03 && type === 0x98) {
            decoded.current_avg = readUInt16LE(bytes, value);
            consumed = 2;
        } else if (channel === 0x04 && type === 0x98) {
            decoded.current_min = readUInt16LE(bytes, value);
            consumed = 2;
        } else if (channel === 0x05 && type === 0x98) {
            decoded.current_max = readUInt16LE(bytes, value);
            consumed = 2;
        } else if (channel === 0x06 && type === 0xc8) {
            decoded.cumulative_charge = readUInt32LE(bytes, value);
            consumed = 4;
        } else if (channel === 0x07 && type === 0x04) {
            decoded.sample_count = readUInt16LE(bytes, value);
            consumed = 2;

        // ---- Configuration echoes ----
        // The device repeats every command it accepted. An echo is the only
        // acknowledgement that a downlink took effect.
        } else if (channel === 0xff && type === 0x02) {
            decoded.sample_interval = readUInt16LE(bytes, value);
            consumed = 2;
        } else if (channel === 0xff && type === 0x03) {
            decoded.report_interval = readUInt16LE(bytes, value);
            consumed = 2;
        } else if (channel === 0xff && type === 0x04) {
            decoded.jitter = readUInt16LE(bytes, value);
            consumed = 2;
        } else if (channel === 0xff && type === 0x10) {
            decoded.reboot = true;
            consumed = 1;
        }

        if (consumed < 0 || value + consumed > bytes.length) {
            decoded.undecoded_at = i;
            break;
        }
        i = value + consumed;
    }

    return decoded;
}

function readClass(value) {
    if (value === 0) return "A";
    if (value === 1) return "B";
    if (value === 2) return "C";
    return "unknown";
}

function readUInt16LE(bytes, offset) {
    return (bytes[offset] | (bytes[offset + 1] << 8)) & 0xffff;
}

function readUInt32LE(bytes, offset) {
    return (
        (bytes[offset] |
            (bytes[offset + 1] << 8) |
            (bytes[offset + 2] << 16) |
            (bytes[offset + 3] << 24)) >>> 0
    );
}

function readHex(bytes, offset, length) {
    var out = "";
    for (var i = 0; i < length; i++) {
        out += ("0" + (bytes[offset + i] & 0xff).toString(16)).slice(-2);
    }
    return out.toUpperCase();
}

// ---- Downlink command encoding ----

/* eslint-disable */
// ChirpStack v4
function encodeDownlink(input) {
    return { bytes: encodeCommands(input.data) };
}

// ChirpStack v3
function Encode(fPort, obj) {
    return encodeCommands(obj);
}

// The Things Network
function Encoder(obj, port) {
    return encodeCommands(obj);
}
/* eslint-enable */

/**
 * Encode configuration commands. Intervals are in seconds.
 *
 *   encodeCommands({ report_interval: 300, sample_interval: 5 })
 *   encodeCommands({ reboot: true })
 *
 * Returns an array of bytes to send on FPort 85. Values outside the ranges in
 * docs/PROTOCOL.md are rejected by the device and will not be echoed back, so
 * they are refused here rather than sent.
 */
function encodeCommands(config) {
    var out = [];
    if (config.sample_interval !== undefined) {
        out = out.concat([0xff, 0x02], u16le(check(config.sample_interval, 1, 3600, "sample_interval")));
    }
    if (config.report_interval !== undefined) {
        out = out.concat([0xff, 0x03], u16le(check(config.report_interval, 10, 65535, "report_interval")));
    }
    if (config.jitter !== undefined) {
        out = out.concat([0xff, 0x04], u16le(check(config.jitter, 0, 600, "jitter")));
    }
    if (config.reboot) {
        out = out.concat([0xff, 0x10, 0xff]);
    }
    return out;
}

function check(value, min, max, name) {
    if (typeof value !== "number" || value !== Math.floor(value) || value < min || value > max) {
        throw new Error(name + " must be a whole number between " + min + " and " + max);
    }
    return value;
}

function u16le(value) {
    return [value & 0xff, (value >> 8) & 0xff];
}

// ---- Standalone CLI ----
//
//   node decode.js <base64|hex>              decode an uplink
//   node decode.js encode report=60 reboot   encode a downlink

if (typeof module !== "undefined" && require.main === module) {
    var arg = process.argv[2];
    if (!arg) {
        console.error("usage: node decode.js <base64|hex>");
        console.error("       node decode.js encode report=<s> sample=<s> jitter=<s> reboot");
        process.exit(1);
    }

    if (arg === "encode") {
        var config = {};
        process.argv.slice(3).forEach(function (item) {
            if (item === "reboot") {
                config.reboot = true;
                return;
            }
            var parts = item.split("=");
            var names = {
                report: "report_interval",
                sample: "sample_interval",
                jitter: "jitter"
            };
            var name = names[parts[0]];
            if (!name) {
                console.error("unknown setting: " + parts[0]);
                process.exit(1);
            }
            config[name] = parseInt(parts[1], 10);
        });
        var bytes = encodeCommands(config);
        console.log("hex:    " + Buffer.from(bytes).toString("hex").toUpperCase());
        console.log("base64: " + Buffer.from(bytes).toString("base64"));
        console.log("fPort:  85");
        process.exit(0);
    }
    var buf = /^[0-9a-fA-F]+$/.test(arg) && arg.length % 2 === 0
        ? Buffer.from(arg, "hex")
        : Buffer.from(arg, "base64");
    console.log(JSON.stringify(decodePayload(Array.from(buf)), null, 2));
}

if (typeof module !== "undefined") {
    module.exports = {
        decodePayload: decodePayload,
        encodeCommands: encodeCommands,
        decodeUplink: decodeUplink,
        encodeDownlink: encodeDownlink
    };
}
