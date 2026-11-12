//! Custom Watt Distribution, Hardware Spec Prompt Generator, AI Response Parser,
//! Currency Catalog (25+ currencies), and Daily Power/Cost Projections.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrencyOption {
    pub code: &'static str,
    pub symbol: &'static str,
    pub label: &'static str,
}

pub const MAJOR_CURRENCIES: &[CurrencyOption] = &[
    CurrencyOption {
        code: "INR",
        symbol: "₹",
        label: "₹ INR (India)",
    },
    CurrencyOption {
        code: "USD",
        symbol: "$",
        label: "$ USD (United States)",
    },
    CurrencyOption {
        code: "EUR",
        symbol: "€",
        label: "€ EUR (Eurozone)",
    },
    CurrencyOption {
        code: "GBP",
        symbol: "£",
        label: "£ GBP (United Kingdom)",
    },
    CurrencyOption {
        code: "JPY",
        symbol: "¥",
        label: "¥ JPY (Japan)",
    },
    CurrencyOption {
        code: "CAD",
        symbol: "C$",
        label: "C$ CAD (Canada)",
    },
    CurrencyOption {
        code: "AUD",
        symbol: "A$",
        label: "A$ AUD (Australia)",
    },
    CurrencyOption {
        code: "CHF",
        symbol: "CHF ",
        label: "CHF (Switzerland)",
    },
    CurrencyOption {
        code: "CNY",
        symbol: "CN¥",
        label: "CN¥ CNY (China)",
    },
    CurrencyOption {
        code: "KRW",
        symbol: "₩",
        label: "₩ KRW (South Korea)",
    },
    CurrencyOption {
        code: "SGD",
        symbol: "S$",
        label: "S$ SGD (Singapore)",
    },
    CurrencyOption {
        code: "HKD",
        symbol: "HK$",
        label: "HK$ HKD (Hong Kong)",
    },
    CurrencyOption {
        code: "NZD",
        symbol: "NZ$",
        label: "NZ$ NZD (New Zealand)",
    },
    CurrencyOption {
        code: "SEK",
        symbol: "kr ",
        label: "kr SEK (Sweden)",
    },
    CurrencyOption {
        code: "NOK",
        symbol: "NOK ",
        label: "NOK (Norway)",
    },
    CurrencyOption {
        code: "PLN",
        symbol: "zł",
        label: "zł PLN (Poland)",
    },
    CurrencyOption {
        code: "BRL",
        symbol: "R$",
        label: "R$ BRL (Brazil)",
    },
    CurrencyOption {
        code: "MXN",
        symbol: "MX$",
        label: "MX$ MXN (Mexico)",
    },
    CurrencyOption {
        code: "ZAR",
        symbol: "R ",
        label: "R ZAR (South Africa)",
    },
    CurrencyOption {
        code: "AED",
        symbol: "AED ",
        label: "AED (UAE)",
    },
    CurrencyOption {
        code: "SAR",
        symbol: "SAR ",
        label: "SAR (Saudi Arabia)",
    },
    CurrencyOption {
        code: "TRY",
        symbol: "₺",
        label: "₺ TRY (Turkey)",
    },
    CurrencyOption {
        code: "PHP",
        symbol: "₱",
        label: "₱ PHP (Philippines)",
    },
    CurrencyOption {
        code: "THB",
        symbol: "฿",
        label: "฿ THB (Thailand)",
    },
    CurrencyOption {
        code: "VND",
        symbol: "₫",
        label: "₫ VND (Vietnam)",
    },
    CurrencyOption {
        code: "IDR",
        symbol: "Rp ",
        label: "Rp IDR (Indonesia)",
    },
];

#[derive(Debug, Clone, Default)]
pub struct DetectedSystemSpecs {
    pub fetched: bool,
    pub os_name: String,
    pub motherboard: String,
    pub cpu_brand: String,
    pub cpu_cores: usize,
    pub cpu_logical_threads: usize,
    pub ram_gb: f32,
    pub ram_details: String,
    pub gpu_name: String,
    pub gpu_vram_gb: f32,
    pub gpu_live_power_w: Option<f32>,
    pub storage_summary: String,
    pub storage_drives: Vec<String>,
    pub monitors: Vec<String>,
    pub network_adapters: Vec<String>,
    pub bluetooth_devices: Vec<String>,
    pub audio_devices: Vec<String>,
    pub usb_peripherals: Vec<String>,
    pub cabinet_cooling: Vec<String>,
    pub is_laptop: bool,
}

/// Trim entries, remove blanks, and deduplicate case-insensitively while preserving order.
pub fn dedupe_clean_list(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for item in items {
        let trimmed = item.split_whitespace().collect::<Vec<_>>().join(" ");
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if !seen.iter().any(|s| s == &lower) {
            seen.push(lower);
            out.push(trimmed);
        }
    }
    out
}

/// Group identical RAM modules (e.g. `16.0 GB DDR4-3000 (CMK16...) + 2x 8.0 GB DDR4-3000`).
pub fn format_ram_sticks_summary(total_gb: f32, sticks: &[(f32, String, u32, String)]) -> String {
    if sticks.is_empty() {
        return if total_gb > 0.0 {
            format!("{:.1} GB", total_gb)
        } else {
            "Unknown".to_string()
        };
    }

    let mut groups: Vec<(String, usize)> = Vec::new();
    for (gb, gen, speed, part) in sticks {
        let cap_str = if (*gb - gb.round()).abs() < 0.15 {
            format!("{:.0} GB", gb.round())
        } else {
            format!("{:.1} GB", gb)
        };
        let mut desc = cap_str;
        match (gen.trim().is_empty(), *speed > 0) {
            (false, true) => desc.push_str(&format!(" {}-{}", gen.trim(), speed)),
            (false, false) => desc.push_str(&format!(" {}", gen.trim())),
            (true, true) => desc.push_str(&format!(" {} MHz", speed)),
            (true, false) => {}
        }
        let clean_part = part.trim();
        if !clean_part.is_empty()
            && !clean_part.eq_ignore_ascii_case("unknown")
            && !clean_part.eq_ignore_ascii_case("0000")
        {
            desc.push_str(&format!(" ({})", clean_part));
        }

        if let Some(existing) = groups.iter_mut().find(|(d, _)| d == &desc) {
            existing.1 += 1;
        } else {
            groups.push((desc, 1));
        }
    }

    let modules_str = groups
        .into_iter()
        .map(|(desc, count)| {
            if count > 1 {
                format!("{}x {}", count, desc)
            } else {
                desc
            }
        })
        .collect::<Vec<_>>()
        .join(" + ");

    if total_gb > 0.0 {
        format!("{:.1} GB total [{}]", total_gb, modules_str)
    } else {
        modules_str
    }
}

#[derive(Debug, Clone)]
pub struct WattCalculatorState {
    pub cpu_idle_w: f32,
    pub gpu_idle_w: f32,
    pub mobo_ram_ssd_fans_w: f32,
    pub peripherals_w: f32,
    pub monitor_each_w: f32,
    pub monitor_count: u32,
    pub psu_wattage: u32,
    pub psu_efficiency_pct: f32,
    pub sleep_watts: f32,
    pub wall_meter_override_w: f32,

    pub smps_model_note: String,
    pub monitor_models_note: String,
    pub extra_gear_note: String,

    pub case_fan_count: u32,
    pub argb_fan_count: u32,
    pub cooler_type: String,
    pub has_rgb_strips_or_hub: bool,

    pub daily_active_hours: f32,
    pub daily_idle_sleep_hours: f32,

    pub generated_prompt: String,
    pub ai_response_input: String,
    pub ai_notes: String,
    pub ai_parse_status: Option<String>,
    pub spec_fetch_in_progress: bool,
    pub spec_edit_target: Option<(String, usize)>,
    pub spec_edit_buf: String,
    pub spec_add_cat: String,
    pub spec_add_buf: String,
}

impl Default for WattCalculatorState {
    fn default() -> Self {
        Self {
            cpu_idle_w: 22.0,
            gpu_idle_w: 18.0,
            mobo_ram_ssd_fans_w: 28.0,
            peripherals_w: 10.0,
            monitor_each_w: 25.0,
            monitor_count: 1,
            psu_wattage: 550,
            psu_efficiency_pct: 85.0,
            sleep_watts: 3.0,
            wall_meter_override_w: 0.0,

            smps_model_note: "550W 80+ Bronze ATX SMPS".to_string(),
            monitor_models_note: "1x 24\" 1080p IPS monitor (~25W)".to_string(),
            extra_gear_note: "Keyboard, mouse, speakers / USB DAC".to_string(),

            case_fan_count: 2,
            argb_fan_count: 0,
            cooler_type: "Stock / Tower Air Cooler".to_string(),
            has_rgb_strips_or_hub: false,

            daily_active_hours: 8.0,
            daily_idle_sleep_hours: 10.0,

            generated_prompt: String::new(),
            ai_response_input: String::new(),
            ai_notes: String::new(),
            ai_parse_status: None,
            spec_fetch_in_progress: false,
            spec_edit_target: None,
            spec_edit_buf: String::new(),
            spec_add_cat: "Cabinet / ARGB & Fans".to_string(),
            spec_add_buf: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComputedWattBreakdown {
    pub dc_pc_watts: f32,
    pub wall_pc_watts: f32,
    pub total_monitor_watts: f32,
    pub total_active_watts: f32,
    pub sleep_watts: f32,
}

impl WattCalculatorState {
    pub fn compute_breakdown(&self) -> ComputedWattBreakdown {
        let total_monitor_watts =
            (self.monitor_each_w.max(0.0)) * (self.monitor_count.max(1) as f32);
        let sleep_watts = self.sleep_watts.max(0.5);

        if self.wall_meter_override_w > 5.0 {
            let wall_pc = self.wall_meter_override_w;
            return ComputedWattBreakdown {
                dc_pc_watts: wall_pc * (self.psu_efficiency_pct.clamp(50.0, 98.0) / 100.0),
                wall_pc_watts: wall_pc,
                total_monitor_watts,
                total_active_watts: (wall_pc + total_monitor_watts).round(),
                sleep_watts,
            };
        }

        let dc_pc = self.cpu_idle_w.max(0.0)
            + self.gpu_idle_w.max(0.0)
            + self.mobo_ram_ssd_fans_w.max(0.0)
            + self.peripherals_w.max(0.0);
        let eff = self.psu_efficiency_pct.clamp(50.0, 98.0) / 100.0;
        let wall_pc = dc_pc / eff;

        ComputedWattBreakdown {
            dc_pc_watts: dc_pc,
            wall_pc_watts: wall_pc,
            total_monitor_watts,
            total_active_watts: (wall_pc + total_monitor_watts).round(),
            sleep_watts,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerProjection {
    pub active_watts: f64,
    pub display_off_watts: f64,
    pub sleep_watts: f64,
    pub watts_saved_in_sleep: f64,
    pub daily_baseline_kwh: f64,
    pub daily_with_guard_kwh: f64,
    pub daily_saved_kwh: f64,
    pub daily_saved_cost: f64,
    pub daily_guard_cost: f64,
    pub monthly_saved_cost: f64,
    pub yearly_saved_cost: f64,
    pub yearly_saved_kwh: f64,
    pub yearly_co2_kg: f64,
}

pub fn compute_power_projection(
    active_watts: f32,
    monitor_watts: f32,
    sleep_watts: f32,
    rate_per_kwh: f32,
    co2_kg_per_kwh: f64,
    daily_active_hours: f32,
    daily_idle_sleep_hours: f32,
) -> PowerProjection {
    let active_w = (active_watts as f64).max(1.0);
    let mon_w = (monitor_watts as f64).max(0.0);
    let sleep_w = (sleep_watts as f64).max(0.0).min(active_w);
    let display_off_w = (active_w - mon_w).max(sleep_w);
    let rate = (rate_per_kwh as f64).max(0.0);

    let active_h = (daily_active_hours as f64).clamp(0.0, 24.0);
    let sleep_h = (daily_idle_sleep_hours as f64).clamp(0.0, 24.0 - active_h);

    let watts_saved = (active_w - sleep_w).max(0.0);
    let daily_baseline_kwh = active_w * (active_h + sleep_h) / 1000.0;
    let daily_with_guard_kwh = (active_w * active_h + sleep_w * sleep_h) / 1000.0;
    let daily_saved_kwh = (watts_saved * sleep_h) / 1000.0;

    let daily_saved_cost = daily_saved_kwh * rate;
    let daily_guard_cost = daily_with_guard_kwh * rate;
    let monthly_saved_cost = daily_saved_cost * 30.0;
    let yearly_saved_kwh = daily_saved_kwh * 365.0;
    let yearly_saved_cost = daily_saved_cost * 365.0;
    let yearly_co2_kg = yearly_saved_kwh * co2_kg_per_kwh.max(0.0);

    PowerProjection {
        active_watts: active_w,
        display_off_watts: display_off_w,
        sleep_watts: sleep_w,
        watts_saved_in_sleep: watts_saved,
        daily_baseline_kwh,
        daily_with_guard_kwh,
        daily_saved_kwh,
        daily_saved_cost,
        daily_guard_cost,
        monthly_saved_cost,
        yearly_saved_cost,
        yearly_saved_kwh,
        yearly_co2_kg,
    }
}

pub fn build_ai_watt_prompt(
    specs: &DetectedSystemSpecs,
    calc: &WattCalculatorState,
    currency_symbol: &str,
    rate_per_kwh: f32,
) -> String {
    let monitors_detected = if specs.monitors.is_empty() {
        "Not auto-detected".to_string()
    } else {
        specs.monitors.join(", ")
    };
    let gpu_pwr = specs
        .gpu_live_power_w
        .map(|w| format!("{:.1} W current board draw", w))
        .unwrap_or_else(|| "N/A".to_string());
    let ram_line = if !specs.ram_details.is_empty() {
        specs.ram_details.clone()
    } else if specs.ram_gb > 0.0 {
        format!("{:.1} GB", specs.ram_gb)
    } else {
        "Unknown".to_string()
    };
    let storage_line = if !specs.storage_drives.is_empty() {
        specs.storage_drives.join(" | ")
    } else if !specs.storage_summary.is_empty() {
        specs.storage_summary.clone()
    } else {
        "Unknown".to_string()
    };
    let network_line = if specs.network_adapters.is_empty() {
        "Not detected".to_string()
    } else {
        specs.network_adapters.join(" | ")
    };
    let bt_line = if specs.bluetooth_devices.is_empty() {
        "None detected".to_string()
    } else {
        specs.bluetooth_devices.join(", ")
    };
    let audio_line = if specs.audio_devices.is_empty() {
        "Default / None detected".to_string()
    } else {
        specs.audio_devices.join(", ")
    };
    let periph_line = if specs.usb_peripherals.is_empty() {
        "Standard keyboard/mouse".to_string()
    } else {
        specs.usb_peripherals.join(", ")
    };
    let cabinet_line = {
        let mut parts = vec![
            format!("CPU Cooler: {}", calc.cooler_type),
            format!("Case Fans: {}", calc.case_fan_count),
            format!("ARGB/RGB Fans: {}", calc.argb_fan_count),
        ];
        if calc.has_rgb_strips_or_hub {
            parts.push("ARGB Controller Hub / LED Light Strips: Yes".to_string());
        }
        if !specs.cabinet_cooling.is_empty() {
            parts.push(format!(
                "Detected/Added: {}",
                specs.cabinet_cooling.join(", ")
            ));
        }
        parts.join(" | ")
    };
    let cpu_threads = specs.cpu_logical_threads.max(specs.cpu_cores);

    format!(
        "Estimate accurate wall-plug power consumption (watts) for my computer so I can configure Agent Power Guard.\n\n\
Detected System Specs:\n\
- OS: {}\n\
- Form Factor: {}\n\
- Motherboard / System: {}\n\
- CPU: {} ({} cores / {} threads)\n\
- RAM: {}\n\
- GPU: {} ({:.1} GB VRAM, {})\n\
- Physical Storage Drives: {}\n\
- Detected Displays: {}\n\
- Network (Wi-Fi / LAN): {}\n\
- Bluetooth: {}\n\
- Audio / Mic / DAC: {}\n\
- Mouse / Keyboard / USB Peripherals: {}\n\
- Cabinet Cooling, Fans & ARGB Lighting: {}\n\n\
Manual Hardware & Usage Inputs:\n\
- SMPS / PSU: {} ({}W, ~{:.0}% efficiency)\n\
- Monitor count & models: {} ({} monitor(s))\n\
- Additional gear / notes: {}\n\
- Electricity rate: {}{:.2}/kWh\n\
- Typical usage: {:.1}h active/day, {:.1}h idle that should sleep/day\n\n\
Reply ONLY with a single JSON object using these exact keys (numbers in watts / tariff, no extra markdown):\n\
{{\n\
  \"active_watts\": 115,\n\
  \"monitor_watts\": 25,\n\
  \"sleep_watts\": 3,\n\
  \"cpu_idle_w\": 20,\n\
  \"gpu_idle_w\": 18,\n\
  \"mobo_ram_ssd_fans_w\": 28,\n\
  \"peripherals_w\": 8,\n\
  \"psu_efficiency_pct\": 85,\n\
  \"currency_symbol\": \"{}\",\n\
  \"electricity_rate_kwh\": {:.2},\n\
  \"notes\": \"Brief component-by-component idle wattage summary\"\n\
}}",
        if specs.os_name.is_empty() { "Unknown" } else { &specs.os_name },
        if specs.is_laptop { "Laptop" } else { "Desktop" },
        if specs.motherboard.is_empty() { "Unknown" } else { &specs.motherboard },
        if specs.cpu_brand.is_empty() { "Unknown CPU" } else { &specs.cpu_brand },
        specs.cpu_cores,
        cpu_threads,
        ram_line,
        if specs.gpu_name.is_empty() { "Integrated / Unknown" } else { &specs.gpu_name },
        specs.gpu_vram_gb,
        gpu_pwr,
        storage_line,
        monitors_detected,
        network_line,
        bt_line,
        audio_line,
        periph_line,
        cabinet_line,
        calc.smps_model_note,
        calc.psu_wattage,
        calc.psu_efficiency_pct,
        calc.monitor_models_note,
        calc.monitor_count.max(1),
        calc.extra_gear_note,
        currency_symbol,
        rate_per_kwh,
        calc.daily_active_hours,
        calc.daily_idle_sleep_hours,
        currency_symbol,
        rate_per_kwh,
    )
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedAiWattResponse {
    pub active_watts: Option<f32>,
    pub monitor_watts: Option<f32>,
    pub sleep_watts: Option<f32>,
    pub cpu_idle_w: Option<f32>,
    pub gpu_idle_w: Option<f32>,
    pub mobo_ram_ssd_fans_w: Option<f32>,
    pub peripherals_w: Option<f32>,
    pub psu_efficiency_pct: Option<f32>,
    pub currency_symbol: Option<String>,
    pub electricity_rate_kwh: Option<f32>,
    pub notes: Option<String>,
}

/// Extract the first `{ ... }` JSON object from raw text or Markdown fenced response.
pub fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&raw[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_json_number(json: &str, keys: &[&str]) -> Option<f32> {
    for key in keys {
        let needle = format!("\"{}\"", key);
        if let Some(pos) = json.find(&needle) {
            let after = &json[pos + needle.len()..];
            if let Some(colon) = after.find(':') {
                let mut val_slice = after[colon + 1..].trim_start();
                if let Some(stripped) = val_slice.strip_prefix('"') {
                    val_slice = stripped;
                }
                let num_end = val_slice
                    .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
                    .unwrap_or(val_slice.len());
                if num_end > 0 {
                    if let Ok(v) = val_slice[..num_end].parse::<f32>() {
                        if v.is_finite() {
                            return Some(v);
                        }
                    }
                }
            }
        }
    }
    None
}

fn find_json_string(json: &str, keys: &[&str]) -> Option<String> {
    for key in keys {
        let needle = format!("\"{}\"", key);
        if let Some(pos) = json.find(&needle) {
            let after = &json[pos + needle.len()..];
            if let Some(colon) = after.find(':') {
                let val_slice = after[colon + 1..].trim_start();
                if let Some(rest) = val_slice.strip_prefix('"') {
                    let mut out = String::new();
                    let mut esc = false;
                    for ch in rest.chars() {
                        if esc {
                            match ch {
                                'n' => out.push('\n'),
                                't' => out.push(' '),
                                _ => out.push(ch),
                            }
                            esc = false;
                        } else if ch == '\\' {
                            esc = true;
                        } else if ch == '"' {
                            return Some(out);
                        } else {
                            out.push(ch);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Normalize AI currency code/symbol (e.g. `"INR"` -> `"₹"`, `"USD"` -> `"$"`).
pub fn normalize_currency_input(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(found) = MAJOR_CURRENCIES
        .iter()
        .find(|c| c.code.eq_ignore_ascii_case(trimmed) || c.symbol.trim() == trimmed)
    {
        return found.symbol.to_string();
    }
    trimmed.to_string()
}

/// Parse an AI-generated JSON reply (even when wrapped in ```json fences or explanatory text).
pub fn parse_ai_watt_response(raw: &str) -> Result<ParsedAiWattResponse, String> {
    let json = extract_json_object(raw.trim()).ok_or_else(|| {
        "No JSON object `{ ... }` found. Paste the AI's JSON response block.".to_string()
    })?;

    let mut out = ParsedAiWattResponse {
        active_watts: find_json_number(
            json,
            &[
                "active_watts",
                "total_active_watts",
                "pc_watts",
                "idle_watts",
            ],
        ),
        monitor_watts: find_json_number(
            json,
            &["monitor_watts", "display_watts", "monitors_watts"],
        ),
        sleep_watts: find_json_number(json, &["sleep_watts", "standby_watts", "s3_watts"]),
        cpu_idle_w: find_json_number(json, &["cpu_idle_w", "cpu_watts"]),
        gpu_idle_w: find_json_number(json, &["gpu_idle_w", "gpu_watts"]),
        mobo_ram_ssd_fans_w: find_json_number(
            json,
            &["mobo_ram_ssd_fans_w", "system_board_w", "motherboard_w"],
        ),
        peripherals_w: find_json_number(json, &["peripherals_w", "extras_w"]),
        psu_efficiency_pct: find_json_number(json, &["psu_efficiency_pct", "efficiency_pct"]),
        currency_symbol: find_json_string(json, &["currency_symbol", "currency"])
            .map(|s| normalize_currency_input(&s))
            .filter(|s| !s.is_empty()),
        electricity_rate_kwh: find_json_number(
            json,
            &[
                "electricity_rate_kwh",
                "unit_price",
                "rate_per_kwh",
                "tariff",
            ],
        ),
        notes: find_json_string(json, &["notes", "summary", "explanation"]),
    };

    if let Some(ref mut w) = out.active_watts {
        *w = w.clamp(10.0, 2000.0);
    }
    if let Some(ref mut w) = out.monitor_watts {
        *w = w.clamp(0.0, 500.0);
    }
    if let Some(ref mut w) = out.sleep_watts {
        *w = w.clamp(0.0, 100.0);
    }
    if let Some(ref mut e) = out.psu_efficiency_pct {
        *e = e.clamp(50.0, 98.0);
    }
    if let Some(ref mut r) = out.electricity_rate_kwh {
        *r = r.clamp(0.01, 50000.0);
    }

    if out.active_watts.is_none()
        && out.monitor_watts.is_none()
        && out.sleep_watts.is_none()
        && out.cpu_idle_w.is_none()
        && out.electricity_rate_kwh.is_none()
    {
        return Err(
            "JSON found, but none of the expected keys (`active_watts`, `monitor_watts`, `sleep_watts`, `electricity_rate_kwh`) were present."
                .to_string(),
        );
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_major_currencies_has_20_plus_and_inr() {
        assert!(MAJOR_CURRENCIES.len() >= 20);
        assert!(MAJOR_CURRENCIES
            .iter()
            .any(|c| c.code == "INR" && c.symbol == "₹"));
        assert_eq!(normalize_currency_input("INR"), "₹");
        assert_eq!(normalize_currency_input("USD"), "$");
        assert_eq!(normalize_currency_input("BDT "), "BDT");
    }

    #[test]
    fn test_watt_distribution_efficiency_and_monitor_math() {
        let calc = WattCalculatorState {
            cpu_idle_w: 20.0,
            gpu_idle_w: 15.0,
            mobo_ram_ssd_fans_w: 25.0,
            peripherals_w: 8.0, // dc = 68W; at 85% eff = 80W wall PC
            monitor_each_w: 25.0,
            monitor_count: 1,
            psu_efficiency_pct: 85.0,
            sleep_watts: 3.0,
            wall_meter_override_w: 0.0,
            ..Default::default()
        };
        let b = calc.compute_breakdown();
        assert!((b.dc_pc_watts - 68.0).abs() < 0.01);
        assert!((b.wall_pc_watts - 80.0).abs() < 0.01);
        assert!((b.total_monitor_watts - 25.0).abs() < 0.01);
        assert!((b.total_active_watts - 105.0).abs() < 0.01);
    }

    #[test]
    fn test_daily_and_yearly_power_projection_inr_9() {
        let p = compute_power_projection(115.0, 25.0, 3.0, 9.0, 0.713, 8.0, 10.0);
        // Saved watts = 112W; over 10h/day = 1.12 kWh/day; at ₹9 = ₹10.08/day
        assert!((p.watts_saved_in_sleep - 112.0).abs() < 0.01);
        assert!((p.daily_saved_kwh - 1.12).abs() < 0.001);
        assert!((p.daily_saved_cost - 10.08).abs() < 0.01);
        assert!((p.monthly_saved_cost - 302.4).abs() < 0.1);
        assert!((p.display_off_watts - 90.0).abs() < 0.01);
    }

    #[test]
    fn test_ai_prompt_and_markdown_fenced_json_parser() {
        let specs = DetectedSystemSpecs {
            fetched: true,
            os_name: "Windows 11".to_string(),
            motherboard: "Gigabyte B550M DS3H R2".to_string(),
            cpu_brand: "Ryzen 5 5600".to_string(),
            cpu_cores: 6,
            cpu_logical_threads: 12,
            ram_gb: 32.0,
            ram_details: "32.0 GB total [16 GB DDR4-3000 + 2x 8 GB DDR4-3000]".to_string(),
            gpu_name: "RTX 3060".to_string(),
            gpu_vram_gb: 12.0,
            gpu_live_power_w: Some(14.5),
            storage_summary: "1x 1TB NVMe".to_string(),
            storage_drives: vec!["WD Blue SN570 1TB (NVMe SSD, 932 GB)".to_string()],
            monitors: vec!["Acer VG240Y (1920x1080)".to_string()],
            network_adapters: vec!["LAN: Realtek PCIe GbE [Connected]".to_string()],
            bluetooth_devices: vec!["TP-Link Bluetooth USB Adapter".to_string()],
            audio_devices: vec!["Microphone (Blue Snowball)".to_string()],
            usb_peripherals: vec!["Dell KB216 Wired Keyboard".to_string()],
            cabinet_cooling: vec!["3x 120mm ARGB Case Fans".to_string()],
            is_laptop: false,
        };
        let mut calc = WattCalculatorState::default();
        calc.case_fan_count = 4;
        calc.argb_fan_count = 3;
        calc.cooler_type = "240mm AIO Liquid Cooler".to_string();
        let prompt = build_ai_watt_prompt(&specs, &calc, "₹", 9.0);
        assert!(prompt.contains("Ryzen 5 5600"));
        assert!(prompt.contains("Gigabyte B550M DS3H R2"));
        assert!(prompt.contains("WD Blue SN570 1TB"));
        assert!(prompt.contains("Blue Snowball"));
        assert!(prompt.contains("ARGB/RGB Fans: 3"));
        assert!(prompt.contains("240mm AIO Liquid Cooler"));
        assert!(prompt.contains("₹9.00/kWh"));

        let ai_reply = "Here is your estimated breakdown:\n```json\n{\n  \"active_watts\": 115,\n  \"monitor_watts\": 25,\n  \"sleep_watts\": 3,\n  \"currency_symbol\": \"INR\",\n  \"electricity_rate_kwh\": 9.0,\n  \"notes\": \"Ryzen 5600 + RTX 3060 idle + Acer VG240Y\"\n}\n```\nHope this helps!";
        let parsed = parse_ai_watt_response(ai_reply).expect("should parse fenced JSON");
        assert_eq!(parsed.active_watts, Some(115.0));
        assert_eq!(parsed.monitor_watts, Some(25.0));
        assert_eq!(parsed.sleep_watts, Some(3.0));
        assert_eq!(parsed.currency_symbol.as_deref(), Some("₹"));
        assert_eq!(parsed.electricity_rate_kwh, Some(9.0));
    }

    #[test]
    fn test_ram_formatter_and_dedupe_clean_list() {
        let sticks = vec![
            (16.0, "DDR4".to_string(), 3000, "CMK16GX4".to_string()),
            (8.0, "DDR4".to_string(), 3000, "CMK8GX4".to_string()),
            (8.0, "DDR4".to_string(), 3000, "CMK8GX4".to_string()),
        ];
        let summary = format_ram_sticks_summary(31.9, &sticks);
        assert!(summary.contains("31.9 GB total"));
        assert!(summary.contains("16 GB DDR4-3000 (CMK16GX4)"));
        assert!(summary.contains("2x 8 GB DDR4-3000 (CMK8GX4)"));

        let cleaned = dedupe_clean_list(vec![
            "  Blue   Snowball ".to_string(),
            "".to_string(),
            "blue snowball".to_string(),
            "Dell KB216".to_string(),
        ]);
        assert_eq!(cleaned, vec!["Blue Snowball", "Dell KB216"]);
    }
}
