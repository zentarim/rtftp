# Project RTFTP

**RTFTP** is a TFTP server powered by the [**libguestfs**](https://libguestfs.org/) project, designed to serve files directly from remote disks over NBD. It enables serving NBD-placed boot-related data—such as kernel images, initrd files, or GRUB configurations—via the TFTP protocol. This approach makes it possible to update or modify boot data (e.g., kernel upgrades or initrd rebuilds) directly from within the device that uses the filesystem itself.

This is particularly useful for managing clusters of diskless bare-metal servers or TFTP-boot capable ARM boards such as the Raspberry Pi.

---

## Description

RTFTP serves each connected client IP address from a unique directory inside the `tftp_root`, similarly to DNSMASQ’s `--tftp-unique-root` option.

To enable a client with IP `X.X.X.X` to receive files from a remote NBD disk, create a JSON file named `X.X.X.X.nbd` inside the `tftp_root` directory with the following structure:

```json
{
    "url": "nbd://<NBD server host>:<port>/<NBD share name>",
    "mounts": [
        {
            "partition": 2,
            "mountpoint": "/"
        },
        {
            "partition": 1,
            "mountpoint": "/boot"
        }
    ],
    "tftp_root": "/boot"
}
```

### Field Explanations:

- **`url`**: The NBD server URL. See the [official NBD URI format](https://github.com/NetworkBlockDevice/nbd/blob/master/doc/uri.md).
- **`mounts`**: An ordered list of mount instructions to build a virtual filesystem from which files are served.
    - Mount the 2nd partition as `/`.
    - Mount the 1st partition as `/boot`.
- **`tftp_root`**: The virtual chroot for the TFTP server. A read request for `kernel.img` will resolve to `/boot/kernel.img` within the virtual FS.

---

## Example

TFTP root directory layout:

```
tftp_root/
├── 192.168.10.10/
│   ├── efi/
│   │   └── grubnetaa64.efi.signed
│   └── grub/
│       └── grub.cfg
└── 192.168.10.10.nbd
```

Contents of `192.168.10.10.nbd`:

```json
{
    "url": "nbd://10.10.10.10:25000/server_root",
    "mounts": [
        {
            "partition": 2,
            "mountpoint": "/"
        },
        {
            "partition": 1,
            "mountpoint": "/boot"
        }
    ],
    "tftp_root": "/boot"
}
```

In this example:

- The client with IP `192.168.10.10` will receive `efi/grubnetaa64.efi.signed` and `grub/grub.cfg` from the **local filesystem**.
- Any other files will be retrieved from the **remote NBD disk** at `nbd://10.10.10.10:25000/server_root` from inside `/boot` directory from the **first** partition.

---

### Notes:

- Only Read Request (RRQ) is supported.
- If a file exists in both the local directory and the NBD-based filesystem, the **local file takes precedence**.
- Initial setup of the virtual NBD filesystem takes **1.5 to 3 seconds**, so the first request usually need to be retried automatically by the client.
- The NBD disk is either:
  - Connected proactively when config is created to avoid the first read request delay.
  - Connected lazily on the first read request.
- An inactive NBD disk is automatically disconnected after a period of inactivity. This timeout is configurable via the `idle_timeout` daemon argument.
- Supported TFTP options:
    - timeout 
    - blksize
    - tsize
- The daemon is intended to run without root privileges. To allow RTFTP to bind to UDP port 69, one of following workarounds may be applied:
    - Add **CAP_NET_BIND_SERVICE** capability to RTFTP: `setcap 'cap_net_bind_service=+ep' /path/to/rtftp`
    - Start RTFTP via `authbind` with port 69 allowed for the RTFTP user: `touch /etc/authbind/byport/69 && chown <rtftp_user>:<rtftp_group> /etc/authbind/byport/69`
