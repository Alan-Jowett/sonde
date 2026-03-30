// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors
//
// WiX custom action that scans for ESP32-S3 modem (VID 303A, PID 1001)
// and populates the MODEM_PORT MSI property with the detected COM port.

using System.Management;
using WixToolset.Dtf.WindowsInstaller;

namespace SondeCustomActions
{
    public class DetectModem
    {
        /// <summary>
        /// Scans USB serial devices for the ESP32-S3 modem (VID 303A, PID 1001)
        /// and sets the MODEM_PORT property to the first matching COM port.
        /// If no device is found, MODEM_PORT is left empty and the operator
        /// must enter the port manually in the dialog.
        /// </summary>
        [CustomAction]
        public static ActionResult DetectModemPort(Session session)
        {
            session.Log("DetectModemPort: scanning for ESP32-S3 modem (VID 303A, PID 1001)");

            try
            {
                // Query Win32_PnPEntity for USB serial devices matching the
                // ESP32-S3 TinyUSB CDC ACM VID/PID.
                using var searcher = new ManagementObjectSearcher(
                    "SELECT * FROM Win32_PnPEntity WHERE DeviceID LIKE '%VID_303A&PID_1001%'");

                foreach (var device in searcher.Get())
                {
                    var deviceId = device["DeviceID"]?.ToString() ?? "";
                    var name = device["Name"]?.ToString() ?? "";
                    session.Log($"DetectModemPort: found device: {name} ({deviceId})");

                    // Extract COM port from the Name field (e.g., "USB Serial Device (COM5)")
                    var comPort = ExtractComPort(name);
                    if (!string.IsNullOrEmpty(comPort))
                    {
                        session.Log($"DetectModemPort: detected modem on {comPort}");
                        session["MODEM_PORT"] = comPort;
                        return ActionResult.Success;
                    }
                }

                // No matching device found — try the registry approach as fallback
                session.Log("DetectModemPort: no device found via WMI, trying registry");
                var registryPort = DetectViaRegistry();
                if (!string.IsNullOrEmpty(registryPort))
                {
                    session.Log($"DetectModemPort: detected modem on {registryPort} via registry");
                    session["MODEM_PORT"] = registryPort;
                    return ActionResult.Success;
                }

                session.Log("DetectModemPort: no ESP32-S3 modem detected");
            }
            catch (System.Exception ex)
            {
                session.Log($"DetectModemPort: error during detection: {ex}");
                // Non-fatal — operator can enter the port manually
            }

            // No modem found — warn the operator with diagnostics.
            // MSI message box: Warning icon, OK/Cancel buttons.
            // MB_OKCANCEL = 0x01, combined with InstallMessage.Warning via bitwise OR.
            const int MB_OKCANCEL = 1;
            using var record = new Record(0);
            record[0] =
                "No ESP32-S3 modem detected.\n\n" +
                "The installer scanned for USB devices with VID 303A / PID 1001 " +
                "(Espressif TinyUSB CDC ACM) but found no matching COM port.\n\n" +
                "Possible causes:\n" +
                "  \u2022 The modem is not plugged in\n" +
                "  \u2022 The modem is on a different USB port\n" +
                "  \u2022 Windows has not installed the usbser.sys driver\n\n" +
                "The gateway service will be installed but may fail to start " +
                "without a valid COM port. You can reconfigure later with:\n\n" +
                "  sonde-gateway install --port COMx\n\n" +
                "Click OK to continue installation, or Cancel to abort.";
            var msgResult = session.Message(
                InstallMessage.Warning | (InstallMessage)MB_OKCANCEL,
                record);
            if (msgResult == MessageResult.Cancel)
                return ActionResult.UserExit;

            return ActionResult.Success;
        }

        private static string ExtractComPort(string deviceName)
        {
            // Name format: "USB Serial Device (COM5)" or similar
            var match = System.Text.RegularExpressions.Regex.Match(
                deviceName, @"\(COM(\d+)\)");
            return match.Success ? $"COM{match.Groups[1].Value}" : null;
        }

        private static string DetectViaRegistry()
        {
            try
            {
                // Enumerate HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001
                using var usbKey = Microsoft.Win32.Registry.LocalMachine.OpenSubKey(
                    @"SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001");
                if (usbKey == null) return null;

                foreach (var serialNumber in usbKey.GetSubKeyNames())
                {
                    using var instanceKey = usbKey.OpenSubKey(serialNumber);
                    if (instanceKey == null) continue;

                    using var paramsKey = instanceKey.OpenSubKey("Device Parameters");
                    if (paramsKey == null) continue;

                    var portName = paramsKey.GetValue("PortName")?.ToString();
                    if (!string.IsNullOrEmpty(portName))
                        return portName;
                }
            }
            catch
            {
                // Registry access may fail — non-fatal
            }
            return null;
        }
    }
}
