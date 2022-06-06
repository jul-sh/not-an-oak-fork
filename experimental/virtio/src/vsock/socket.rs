//
// Copyright 2022 The Project Oak Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

use alloc::collections::VecDeque;

use super::{
    packet::{Packet, VSockFlags, VSockOp, HEADER_SIZE},
    VSock, DATA_BUFFER_SIZE, QUEUE_SIZE,
};
use core::num::Wrapping;

/// The maximum buffer size used by the socket.
///
/// This is used for flow-control calculations. For now we use the maximum size seeing that we don't
/// have a fixed limit and don't want to send too many credit update packets.
const STREAM_BUFFER_LENGTH: Wrapping<u32> = Wrapping(u32::MAX);

/// The limit that triggers a voluntary credit update message to avoid stalling.
///
/// If the peer's calculation of our free buffer space falls below this point (e.g when we receive a
/// lot of data without sending any packets back) we send a credit update packet to make sure the
/// peer knows we have more space available.
const CREDIT_UPDATE_LIMIT: Wrapping<u32> = Wrapping((DATA_BUFFER_SIZE * QUEUE_SIZE) as u32);

/// The maximum size of the payload of a single packet to ensure it fits into a single buffer in the
/// queue.
const MAX_PAYLOAD_SIZE: usize = DATA_BUFFER_SIZE - HEADER_SIZE;

/// Connector to initiate a connection to a listener on the host.
pub struct SocketConnector {
    /// The socket configuration.
    config: SocketConfiguration,
}

impl SocketConnector {
    pub fn new(vsock: VSock, host_port: u32, local_port: u32) -> Self {
        Self {
            config: SocketConfiguration::new(vsock, local_port, host_port),
        }
    }

    /// Tries to connect to a listener on the host.
    ///
    /// Since we don't yet support timeouts it will wait indefinitely for a respone. If the
    /// connection is refused, or it receives an unexpected packet, it will return an error.
    pub fn connect(mut self) -> anyhow::Result<Socket> {
        let mut packet = Packet::new_control(
            self.config.local_port,
            self.config.host_port,
            VSockOp::Request,
        )?;
        // Set credit info.
        packet.set_buf_alloc(STREAM_BUFFER_LENGTH.0);
        packet.set_fwd_cnt(0);
        self.config.vsock.write_packet(&mut packet);
        let src_port = self.config.host_port;
        let dst_port = self.config.local_port;
        loop {
            if let Some(packet) = self.config.vsock.read_filtered_packet(
                |packet| packet.get_dst_port() == dst_port && packet.get_src_port() == src_port,
                true,
            ) {
                if packet.get_op()? == VSockOp::Response {
                    break;
                } else {
                    anyhow::bail!(
                        "Invalid response to connection request: {}",
                        packet.get_op()?
                    );
                }
            }
        }
        Ok(Socket::new(self.config))
    }
}

/// Listener that waits for a connection initiated from the host.
pub struct SocketListener {
    /// The socket configuration.
    config: SocketConfiguration,
}

impl SocketListener {
    pub fn new(vsock: VSock, port: u32) -> Self {
        Self {
            config: SocketConfiguration::new(vsock, port, 0),
        }
    }

    /// Listens for a connection from the host on the specified port.
    ///
    /// Since we don't yet support timeouts it will wait indefinitely for a connection request. If
    /// it receives an unexpected packet (anything other than a connection request) it will return
    /// an error.
    pub fn accept(mut self) -> anyhow::Result<Socket> {
        let dst_port = self.config.local_port;
        loop {
            if let Some(packet) = self
                .config
                .vsock
                .read_filtered_packet(|packet| packet.get_dst_port() == dst_port, true)
            {
                if packet.get_op()? == VSockOp::Request {
                    self.config.host_port = packet.get_src_port();
                    break;
                } else {
                    anyhow::bail!("Invalid connection request: {}", packet.get_op()?);
                }
            }
        }

        let mut packet = Packet::new_control(
            self.config.local_port,
            self.config.host_port,
            VSockOp::Response,
        )?;
        // Set credit info.
        packet.set_buf_alloc(STREAM_BUFFER_LENGTH.0);
        packet.set_fwd_cnt(0);
        self.config.vsock.write_packet(&mut packet);

        Ok(Socket::new(self.config))
    }
}

/// A connection-oriented socket.
pub struct Socket {
    /// The socket configuration.
    config: SocketConfiguration,
    /// The current state of the connection.
    connection_state: ConnectionState,
    /// The number of payload bytes we have processed.
    ///
    /// This is a free-running counter that wraps around.
    processed_bytes: Wrapping<u32>,
    /// The previous value of `processed_bytes` that was sent to the peer in the most recent packet
    /// header.
    previous_processed_bytes: Wrapping<u32>,
    /// The number of payload bytes we have sent.
    ///
    /// This is a free-running counter that wraps around.
    sent_bytes: Wrapping<u32>,
    /// The number of payload bytes the peer has processed according to the received packet
    /// headers.
    peer_processed_bytes: Wrapping<u32>,
    /// The size of the peer's stream buffer.
    peer_buffer_size: Wrapping<u32>,
    /// A temporary buffer to store extra data from a packet that was not fully read.
    pending_data: Option<VecDeque<u8>>,
}

impl Socket {
    fn new(config: SocketConfiguration) -> Self {
        Self {
            config,
            connection_state: ConnectionState::Connected,
            processed_bytes: Wrapping(0),
            previous_processed_bytes: Wrapping(0),
            sent_bytes: Wrapping(0),
            peer_processed_bytes: Wrapping(0),
            peer_buffer_size: Wrapping(0),
            pending_data: None,
        }
    }

    /// Shuts the connection down.
    ///
    /// At the moment this will cause the vsock driver to be dropped, which means that no future
    /// connections will be possible. This should only be used if no further communications with the
    /// host is expected.
    pub fn shutdown(mut self) {
        if self.connection_state == ConnectionState::Connected {
            let mut packet = Packet::new_control(
                self.config.local_port,
                self.config.host_port,
                VSockOp::Shutdown,
            )
            .expect("Could not create control packet.");
            // Notify the host that we will not send or receive any more data packets.
            packet.set_flags(VSockFlags::all());
            self.config.vsock.write_packet(&mut packet);
        }
    }

    /// Whether we should send an unsolicited credit update.
    fn must_send_credit_update(&self) -> bool {
        STREAM_BUFFER_LENGTH - (self.processed_bytes - self.previous_processed_bytes)
            < CREDIT_UPDATE_LIMIT
    }

    /// Sends a control packet with the specified op to the host.
    fn send_control_packet(&mut self, op: VSockOp) -> anyhow::Result<()> {
        // For now we panic if we are disconnected.
        assert!(self.connection_state == ConnectionState::Connected);
        let mut packet = Packet::new_control(self.config.local_port, self.config.host_port, op)?;
        self.set_credit_info(&mut packet);
        self.config.vsock.write_packet(&mut packet);
        Ok(())
    }

    /// Sends a data packet to the host.
    fn send_data_packet(&mut self, data: &[u8]) -> anyhow::Result<()> {
        // For now we panic if we are disconnected.
        assert!(
            self.connection_state == ConnectionState::Connected,
            "Stream disconnected."
        );
        let data_len = data.len();
        assert!(
            data_len <= MAX_PAYLOAD_SIZE,
            "The data is too large for a single packet. Len: {}, Max: {}",
            data.len(),
            MAX_PAYLOAD_SIZE
        );

        let data_len = Wrapping(data_len as u32);
        if data_len > self.peer_buffer_size - (self.sent_bytes - self.peer_processed_bytes) {
            anyhow::bail!("Peer's stream buffer is full.");
        }

        self.sent_bytes += data_len;
        let mut packet = Packet::new_data(data, self.config.local_port, self.config.host_port)?;
        self.set_credit_info(&mut packet);
        self.config.vsock.write_packet(&mut packet);
        Ok(())
    }

    /// Updates the credit info on a packet to facilitate flow-control.
    fn set_credit_info(&mut self, packet: &mut Packet) {
        packet.set_buf_alloc(STREAM_BUFFER_LENGTH.0);
        packet.set_fwd_cnt(self.processed_bytes.0);
        self.previous_processed_bytes = self.processed_bytes;
    }

    /// Reads the payload of the next available data packet, if any are available.
    fn read_data(&mut self) -> Option<VecDeque<u8>> {
        // For now we panic if we are disconnected.
        assert!(
            self.connection_state == ConnectionState::Connected,
            "Stream disconnected."
        );
        let src_port = self.config.host_port;
        let dst_port = self.config.local_port;
        loop {
            let packet = self.config.vsock.read_filtered_packet(
                |packet| packet.get_dst_port() == dst_port && packet.get_src_port() == src_port,
                true,
            )?;
            self.peer_buffer_size = Wrapping(packet.get_buf_alloc());
            self.peer_processed_bytes = Wrapping(packet.get_fwd_cnt());
            // For now we panic if we receive an invalid op.
            match packet.get_op().expect("Invalid packet received on stream.") {
                VSockOp::CreditRequest => {
                    self.send_control_packet(VSockOp::CreditUpdate)
                        .expect("Could not create control packet.");
                }
                VSockOp::CreditUpdate => {
                    // We already updated our flow-control tracking data, so do nothing.
                }
                VSockOp::Request | VSockOp::Response => {
                    // For now we panic if we receive an invalid op.
                    panic!("Invalid packet received on stream.");
                }
                VSockOp::Rst => {
                    self.connection_state = ConnectionState::Disconnected;
                    return None;
                }
                VSockOp::Shutdown => {
                    self.send_control_packet(VSockOp::Rst)
                        .expect("Could not create control packet.");
                    self.connection_state = ConnectionState::Disconnected;
                    return None;
                }
                VSockOp::Rw => {
                    let data = packet.get_payload();
                    // TODO(#2876): Avoid copying the buffer slice if possible.
                    let mut result = VecDeque::with_capacity(data.len());
                    result.extend(data);
                    return Some(result);
                }
            }
        }
    }

    /// Tries once to fill the destination with as much data as is currently available, either in
    /// the pending buffer or from the next available data packet.
    ///
    /// Returns the number of bytes read if any data was available to read.
    fn read_partial(&mut self, dest: &mut [u8]) -> Option<usize> {
        let mut source = match self.pending_data.take() {
            Some(data) => data,
            None => self.read_data()?,
        };

        let len = dest.len();
        let mut position = 0;
        while position < len && let Some(byte) = source.pop_front() {
            dest[position] = byte;
            position += 1;
        }

        if !source.is_empty() {
            self.pending_data.replace(source);
        }
        Some(position)
    }
}

impl ciborium_io::Read for Socket {
    type Error = anyhow::Error;

    fn read_exact(&mut self, data: &mut [u8]) -> Result<(), Self::Error> {
        let len = data.len();
        let mut count = 0;
        while count < len {
            count += self.read_partial(&mut data[count..]).unwrap_or(0);
        }

        self.processed_bytes += Wrapping(count as u32);

        if self.must_send_credit_update() {
            self.send_control_packet(VSockOp::CreditUpdate)
                .map_err(|error| anyhow::anyhow!("Could not create control packet: {:?}", error))?;
        }

        Ok(())
    }
}

impl ciborium_io::Write for Socket {
    type Error = anyhow::Error;

    fn write_all(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        let mut start = 0;
        let data_len = data.len();
        while start < data_len {
            let end = core::cmp::min(data_len, start + MAX_PAYLOAD_SIZE);
            self.send_data_packet(&data[start..end])?;
            start = end;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // We always flush on write, so do nothing.
        // TODO(#2876): We should use a bufferd writer so that we don't always flush on write, and
        // provide and actual flush implementation here.
        Ok(())
    }
}

/// The state of the connection.
#[derive(PartialEq, Eq)]
enum ConnectionState {
    Connected,
    Disconnected,
}

/// The configuration information for the socket.
struct SocketConfiguration {
    /// The vsock device driver.
    ///
    /// For now we only support one connection, so the driver is owned by this configuration.
    vsock: VSock,
    /// The local port for the connection.
    local_port: u32,
    /// The host port for the connection.
    host_port: u32,
}

impl SocketConfiguration {
    fn new(vsock: VSock, local_port: u32, host_port: u32) -> Self {
        Self {
            vsock,
            local_port,
            host_port,
        }
    }
}
