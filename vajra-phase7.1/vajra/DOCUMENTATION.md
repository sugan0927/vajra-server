
---

## 9. Zero-Copy File Serving (io_uring `splice`)

Vajra is designed for extreme static file delivery performance. Instead of reading files into the application's userspace RAM and then writing them to the network socket, Vajra uses **Linux Zero-Copy** techniques.

### How it works under the hood
For plain HTTP (port 80 or unencrypted traffic), Vajra uses `IORING_OP_SPLICE`.
1. It opens the file on disk.
2. It creates an in-kernel pipe (`libc::pipe2`).
3. It uses `io_uring` to `splice` the file data directly into the pipe.
4. It uses `io_uring` to `splice` the pipe data directly into the network socket.

**Result:** The data never touches the Vajra application's memory. The CPU usage drops to near zero, and data is moved directly from the Disk Controller to the NIC (Network Interface Card).

### The TLS Limitation (and the kTLS Solution)
When serving HTTPS (TLS), the data must be encrypted. In standard user-space TLS (like `rustls`), Vajra *must* read the file into RAM, encrypt it, and then send it. This breaks the Zero-Copy pipeline.

**Future Upgrade (kTLS):**
To enable Zero-Copy for HTTPS, Vajra is architected to support **kTLS (Kernel TLS)**. When kTLS is fully integrated:
1. The TLS handshake happens in userspace (via `rustls`).
2. The socket is offloaded to the Linux Kernel using `setsockopt(fd, SOL_TLS, TLS_TX, ...)`.
3. The Kernel handles the AES-GCM encryption.
4. Vajra can then use `IORING_OP_SPLICE` to push the unencrypted file directly to the TLS socket. The kernel encrypts it on the fly as it leaves the NIC.

### Performance Benchmark (Theoretical)
* **Standard Read/Write:** ~2 GB/s (RAM limited)
* **io_uring Splice:** ~10 GB/s+ (Limited only by Disk and NIC bandwidth)

