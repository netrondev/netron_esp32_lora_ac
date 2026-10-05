/**
 * Worked example: configuring an ESP32 power monitor and confirming it worked.
 *
 * Shows the full cycle — build a downlink, send it, then watch uplinks for the
 * echo that confirms the device applied it.
 *
 * Run it: node example.js
 *
 * It replays payloads captured from a real device, so it demonstrates the
 * whole flow without any hardware. The two pieces worth lifting into your own
 * integration are `classifyUplink` and `PendingCommands`.
 */

var codec = require("./decode.js");

var FPORT = 85;

// ─────────────────────────────────────────────────────────────────────────────
// 1. Building a downlink
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Build a downlink payload from the settings you want to change.
 *
 * Returns hex and base64 — most platforms want one or the other — plus the
 * settings themselves, which you keep so you can recognise the echo later.
 */
function buildDownlink(settings) {
    var bytes = codec.encodeCommands(settings);
    var buffer = Buffer.from(bytes);
    return {
        fPort: FPORT,
        hex: buffer.toString("hex").toUpperCase(),
        base64: buffer.toString("base64"),
        settings: settings
    };
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Making sense of an uplink
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Sort a decoded uplink into one of three kinds.
 *
 * The device sends all three on the same port, so the payload contents are
 * what distinguishes them:
 *   - "device_info"   sent once after every join
 *   - "config_echo"   confirmation that a downlink was applied
 *   - "report"        a periodic measurement report
 */
function classifyUplink(bytes) {
    var data = codec.decodePayload(bytes);

    if (data.sn !== undefined) {
        return { kind: "device_info", data: data };
    }

    var echoed = {};
    var isEcho = false;
    ["report_interval", "sample_interval", "jitter", "reboot"].forEach(function (key) {
        if (data[key] !== undefined) {
            echoed[key] = data[key];
            isEcho = true;
        }
    });
    if (isEcho) {
        return { kind: "config_echo", data: data, echoed: echoed };
    }

    return { kind: "report", data: data };
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Tracking whether a command actually landed
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Keeps track of commands sent but not yet confirmed.
 *
 * This matters because a downlink is not delivered immediately and is not
 * retried. A Class A device only listens briefly after it transmits, so a
 * queued command waits for the device's next report — up to a full report
 * interval — and if it is missed, nothing tells you. The echo is the only
 * confirmation, so hold onto what you sent until it comes back.
 */
function PendingCommands() {
    this.pending = [];
}

/** Record a command as sent. */
PendingCommands.prototype.sent = function (devEui, settings, now) {
    this.pending.push({
        devEui: devEui,
        settings: settings,
        sentAt: now || Date.now()
    });
};

/**
 * Feed every uplink through this. Returns what it resolved, if anything.
 *
 * A command counts as confirmed when the echo carries the same value we asked
 * for. A different value means the device applied something else — worth
 * surfacing rather than treating as success.
 */
PendingCommands.prototype.handleUplink = function (devEui, bytes) {
    var uplink = classifyUplink(bytes);
    if (uplink.kind !== "config_echo") {
        return { uplink: uplink, confirmed: [], mismatched: [] };
    }

    var confirmed = [];
    var mismatched = [];
    var stillPending = [];

    this.pending.forEach(function (item) {
        if (item.devEui !== devEui) {
            stillPending.push(item);
            return;
        }

        var keys = Object.keys(item.settings);
        var matched = keys.filter(function (key) {
            return uplink.echoed[key] !== undefined;
        });

        if (matched.length === 0) {
            stillPending.push(item);
            return;
        }

        var wrong = matched.filter(function (key) {
            // reboot echoes as `true` rather than a value.
            if (key === "reboot") return false;
            return uplink.echoed[key] !== item.settings[key];
        });

        if (wrong.length > 0) {
            mismatched.push({ item: item, echoed: uplink.echoed, keys: wrong });
        } else {
            confirmed.push({ item: item, keys: matched });
        }
    });

    this.pending = stillPending;
    return { uplink: uplink, confirmed: confirmed, mismatched: mismatched };
};

/** Commands that have gone unconfirmed for too long and should be resent. */
PendingCommands.prototype.overdue = function (maxAgeMs, now) {
    now = now || Date.now();
    return this.pending.filter(function (item) {
        return now - item.sentAt > maxAgeMs;
    });
};

// ─────────────────────────────────────────────────────────────────────────────
// Demonstration, using payloads captured from a real device
// ─────────────────────────────────────────────────────────────────────────────

function hex(text) {
    return Buffer.from(text, "hex");
}

function main() {
    var devEui = "E08CFEFFFE34C3AC";
    var pending = new PendingCommands();

    console.log("--- 1. Build the downlink ---\n");

    var downlink = buildDownlink({ report_interval: 300, sample_interval: 5 });
    console.log("Send this on fPort " + downlink.fPort + ":");
    console.log("  hex:    " + downlink.hex);
    console.log("  base64: " + downlink.base64);
    console.log("\nThen record it as pending until the device confirms it:");
    pending.sent(devEui, downlink.settings);
    console.log("  " + pending.pending.length + " command awaiting confirmation");

    console.log("\n--- 2. Uplinks arrive ---\n");

    // The device's next report, sent before our command reached it.
    var report = hex("03981500049807000598260006c83900000007041e00");
    var result = pending.handleUplink(devEui, report);
    describe(result);

    // The echo, in the uplink after the command was delivered.
    var echo = hex("ff032c01ff020500");
    result = pending.handleUplink(devEui, echo);
    describe(result);

    console.log("Remaining unconfirmed: " + pending.pending.length);

    console.log("\n--- 3. A device that has just joined ---\n");

    var info = hex("ff0bffff0101ff090100ff0a0007ff0f00ff16e08cfefffe34c3ac");
    describe(pending.handleUplink(devEui, info));

    console.log("--- 4. Resending what was never confirmed ---\n");

    pending.sent(devEui, { jitter: 10 }, Date.now() - 20 * 60 * 1000);
    var overdue = pending.overdue(15 * 60 * 1000);
    overdue.forEach(function (item) {
        console.log(
            "No echo for " + JSON.stringify(item.settings) + " after 15 minutes — resend:"
        );
        console.log("  hex: " + buildDownlink(item.settings).hex);
    });

    console.log("\n--- 5. Rejected before it is ever sent ---\n");

    try {
        buildDownlink({ report_interval: 5 });
    } catch (e) {
        console.log("  " + e.message);
        console.log("  (the device would silently reject it, so it never goes out)");
    }
}

function describe(result) {
    var uplink = result.uplink;

    if (uplink.kind === "report") {
        console.log(
            "report: " +
                uplink.data.current_avg +
                " mA average (" +
                uplink.data.current_min +
                "–" +
                uplink.data.current_max +
                " mA), " +
                uplink.data.cumulative_charge +
                " mAh total, from " +
                uplink.data.sample_count +
                " samples"
        );
    } else if (uplink.kind === "device_info") {
        console.log(
            "device joined: " +
                uplink.data.sn +
                " firmware " +
                uplink.data.firmware_version +
                ", class " +
                uplink.data.lorawan_class
        );
    } else {
        console.log("config echo: " + JSON.stringify(uplink.echoed));
    }

    result.confirmed.forEach(function (entry) {
        console.log("  confirmed applied: " + entry.keys.join(", "));
    });
    result.mismatched.forEach(function (entry) {
        console.log(
            "  MISMATCH on " +
                entry.keys.join(", ") +
                " — asked for " +
                JSON.stringify(entry.item.settings) +
                ", device reports " +
                JSON.stringify(entry.echoed)
        );
    });
    console.log("");
}

if (require.main === module) {
    main();
}

module.exports = {
    buildDownlink: buildDownlink,
    classifyUplink: classifyUplink,
    PendingCommands: PendingCommands
};
