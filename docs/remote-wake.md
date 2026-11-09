# Remote Wake Outside the Local Network (Wake-on-LAN Guide)

When your workstation is suspended in deep sleep, its CPU, storage, and web server are powered down. To access your computer or trigger tasks remotely, you need to wake it before connecting to the Agent Power Guard remote dashboard or SSH.

This guide explains how to reliably wake your workstation from anywhere in the world.

---

## 1. How Wake-on-LAN (WoL) Works

Wake-on-LAN relies on a **Magic Packet**—a broadcast UDP packet containing 6 bytes of `0xFF` followed by 16 repetitions of the target machine's 48-bit MAC address:

```text
[ FF FF FF FF FF FF ] + [ MAC MAC MAC MAC MAC MAC ] x 16
```

When your computer enters sleep (S3 or Modern Standby), a low-power microcontroller on the Network Interface Card (NIC) stays energized, listening for packets addressed to its physical layer.

---

## 2. Hardware & BIOS Checklist

Wake-on-LAN will not work unless enabled at both the firmware (UEFI/BIOS) and OS driver levels:

### Motherboard / UEFI Settings
1. Enter your BIOS/UEFI setup (typically by pressing `Del` or `F2` at boot).
2. Look for **Power Management Setup** or **Advanced > ACPI Configuration**.
3. Enable **PCIe Power On**, **Power On By PCI-E/PCI Device**, or **Wake on LAN (WoL)**.
4. Disable **ErP Ready** or **Deep Sleep** (ErP shuts down standby power to the NIC, disabling WoL).

### Windows Device Manager
1. Open **Device Manager** (`devmgmt.msc`) -> expand **Network adapters**.
2. Right-click your Ethernet adapter (Intel, Realtek, etc.) -> **Properties**.
3. In the **Advanced** tab:
   - Set **Wake on Magic Packet** to **Enabled**.
   - If present, set **Energy Efficient Ethernet** to **Disabled** (avoids link drops during sleep).
4. In the **Power Management** tab:
   - Check **Allow this device to wake the computer**.
   - Check **Only allow a magic packet to wake the computer**.
5. Disable Windows **Fast Startup**:
   - Open **Power Options** -> **Choose what the power buttons do** -> click **Change settings that are currently unavailable**.
   - Uncheck **Turn on fast startup** (Fast startup puts Windows into a hybrid shutdown state that may cut standby power to some NICs).

### Linux / systemd
Enable magic packet wake on your network interface (e.g., `eth0` or `enp3s0`):
```bash
sudo ethtool -s eth0 wol g
```
To persist across reboots, add `WakeOnLan=magic` in your systemd network unit or create a udev rule.

### macOS
Open **System Settings > Energy Saver** (or **Displays / Battery**) and enable **Wake for network access**.

> [!IMPORTANT]
> **Ethernet vs. Wi-Fi:** Always connect your workstation via wired Ethernet for Wake-on-LAN. Wi-Fi adapters power down their radios when the machine sleeps and cannot reliably receive broadcast magic packets.

---

## 3. Remote Wake Outside the LAN

Because routers drop broadcast packets (`255.255.255.255`) crossing subnet boundaries from the public internet, you cannot simply forward port 9 from your home router.

There are three reliable architectures to wake your PC from anywhere:

### Approach A: Tailscale + Lightweight Always-On Node (Recommended)

Run a low-power, always-on device on your home network (such as a Raspberry Pi, a NAS, a home server, or an Apple TV with Homebridge):

1. Install **Tailscale** on your phone/laptop and on the always-on node.
2. When outside your home Wi-Fi, connect to your Tailscale mesh VPN.
3. SSH into the always-on node (or trigger an automation script) and run:
   ```bash
   # Using Agent Power Guard on the Raspberry Pi / Linux node
   agent-power-guard wol 00:11:22:33:44:55

   # Or using standard Linux etherwake
   sudo etherwake -i eth0 00:11:22:33:44:55
   ```
4. Within 5–10 seconds, your workstation will power on, reconnect to your Tailscale tailnet, and become available in the Agent Power Guard dashboard.

### Approach B: Tailscale Subnet Router

Configure your always-on home node as a **Tailscale Subnet Router**:
```bash
sudo tailscale up --advertise-routes=192.168.1.0/24
```
From your remote device, broadcast magic packets directly to the home subnet's directed broadcast address (e.g. `192.168.1.255`):
```bash
agent-power-guard wol 00:11:22:33:44:55 192.168.1.255
```

### Approach C: Router Web Interface or VPN

Most modern home routers (ASUS ASUSWRT, OpenWrt, DD-WRT, pfSense, OPNsense, Ubiquiti UniFi) have:
1. Built-in **Wake-on-LAN** utilities in their administrative web console or mobile app.
2. Built-in WireGuard / OpenVPN servers that put your mobile device directly on the home subnet.

---

## 4. Built-in `agent-power-guard wol` CLI Subcommand

Agent Power Guard includes a built-in Wake-on-LAN client:

```bash
# Send magic packet to default broadcast (255.255.255.255:9)
agent-power-guard wol 00:11:22:33:44:55

# Send magic packet to a specific subnet broadcast IP
agent-power-guard wol 00:11:22:33:44:55 192.168.1.255
```

The command accepts standard MAC formats (`00:11:22:33:44:55`, `00-11-22-33-44-55`, or unformatted hex `001122334455`).

