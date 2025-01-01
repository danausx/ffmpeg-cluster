# FFmpeg Cluster: Distributed Video Processing

## Overview

FFmpeg Cluster is a distributed video processing system built in Rust that leverages hardware-accelerated FFmpeg encoding across multiple machines. It uses WebSocket communication for real-time coordination and supports automatic hardware detection for optimal performance.

## Key Features

- **Hardware-Accelerated Processing**: Supports multiple encoders:
  - NVIDIA NVENC
  - Intel QuickSync
  - AMD AMF
  - Apple VideoToolbox
  - Intel/AMD VAAPI on Linux
- **Intelligent Load Balancing**: Dynamic workload distribution based on client performance
- **Frame-Accurate Processing**: Precise frame handling during splitting and recombination
- **Automatic Recovery**: Built-in retry mechanisms and error handling
- **Real-time Monitoring**: Live progress tracking and client status updates

## Architecture

The project is composed of three main Rust components:

1. **ffmpeg-cluster-server**: 
   - Coordinates client connections
   - Manages video segment distribution
   - Handles websocket communications
   - Orchestrates the overall encoding process

2. **ffmpeg-cluster-client**:
   - Connects to the server
   - Performs video processing tasks
   - Uploads processed video segments

3. **ffmpeg-cluster-common**:
   - Shared models and error handling
   - Common message structures
   - Configuration management

## Prerequisites

- Rust (latest stable version)
- FFmpeg (automatically managed by the client)

## Configuration

### Server Options
- `--required-clients`: Number of clients needed (default: 3)
- `--benchmark-duration`: Benchmark duration in seconds (default: 10)
- `--ffmpeg-params`: FFmpeg encoding parameters
- `--file-name-output`: Output filename
- `--port`: Server port (default: 5001)
- `--exactly`: Enable exact frame counting (default: true)
- `--chunk-size`: Data chunk size in bytes (default: 1MB)
- `--max-retries`: Maximum retry attempts (default: 3)
- `--retry-delay`: Delay between retries in seconds (default: 5)

### Client Options
- `--server-ip`: Server address (default: localhost)
- `--server-port`: Server port (default: 5001)
- `--benchmark-duration`: Benchmark duration (default: 10)
- `--reconnect-delay`: Reconnection delay in seconds (default: 3)
- `--persistent`: Keep trying to reconnect (default: true)

## Job States

Jobs progress through the following states:
- Queued
- WaitingForClients
- Benchmarking
- Processing
- Combining
- Completed
- Failed
- Cancelled

## Client States

Clients can be in the following states:
- Connected
- Benchmarking
- Processing
- Idle
- Disconnected

## Performance Considerations

- Processing speed depends on:
  - Available hardware acceleration
  - Network bandwidth
  - Input video characteristics
  - Number of connected clients
  - Client hardware capabilities

## Error Handling

The system includes comprehensive error handling for:
- Network disconnections
- Processing failures
- Frame count mismatches
- Hardware acceleration issues
- Corrupt video segments

## Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/AmazingFeature`)
3. Commit your changes (`git commit -m 'Add some AmazingFeature'`)
4. Push to the branch (`git push origin feature/AmazingFeature`)
5. Open a Pull Request