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
- **SQLite Integration**: Client and job tracking with persistent storage
- **Format Detection**: Automatic video format detection and handling
- **Multi-platform Support**: Works on Windows, macOS, and Linux

## Architecture

The project consists of three main Rust components:

1. **ffmpeg-cluster-server**: 
   - Coordinates client connections
   - Manages video segment distribution
   - Handles WebSocket communications
   - Orchestrates the encoding process
   - Manages SQLite database for tracking
   - Provides REST API endpoints

2. **ffmpeg-cluster-client**:
   - Connects to the server
   - Performs video processing tasks
   - Manages local FFmpeg binaries
   - Handles hardware acceleration detection
   - Supports automatic reconnection
   - Maintains persistent client ID

3. **ffmpeg-cluster-common**:
   - Shared models and error types
   - Message structures
   - Configuration management
   - Job and client state definitions

## Prerequisites

- Rust (latest stable version)
- Docker (for cross-compilation)
- curl and unzip (for FFmpeg download)

FFmpeg and FFprobe are automatically downloaded and managed by the build procedure.

## Building

### Build Script Usage

The project includes a comprehensive build script (`build.sh`) that handles:
- Cross-compilation
- FFmpeg binary management
- Platform-specific builds
- Release packaging

```bash
# Build both client and server (default)
./build.sh

# Build client only
./build.sh -c client

# Build server only
./build.sh -c server

# Build for specific platform
./build.sh -p linux-x86_64
./build.sh -p macos-universal2
./build.sh -p windows-x86_64
```

## Running the Server

### Basic Usage

```bash
./ffmpeg-cluster-server
```

### Advanced Configuration

```bash
./ffmpeg-cluster-server \
  --required-clients 3 \
  --benchmark-duration 10 \
  --ffmpeg-params "-c:v libx264 -preset medium" \
  --file-name-output "output.mp4" \
  --port 5001 \
  --exactly true \
  --chunk-size 1048576 \
  --max-retries 3 \
  --retry-delay 5
```

### Server Options

| Option | Description | Default |
|--------|-------------|---------|
| --required-clients | Number of clients needed | 3 |
| --benchmark-duration | Benchmark duration (seconds) | 10 |
| --ffmpeg-params | FFmpeg encoding parameters | -c:v libx264 -preset medium |
| --file-name-output | Output filename | output.mp4 |
| --port | Server port | 5001 |
| --exactly | Enable exact frame counting | true |
| --chunk-size | Data chunk size (bytes) | 1MB |
| --max-retries | Maximum retry attempts | 3 |
| --retry-delay | Delay between retries (seconds) | 5 |

## Running the Client

### Basic Usage

```bash
./ffmpeg-cluster-client
```

### Advanced Configuration

```bash
./ffmpeg-cluster-client \
  --server-ip localhost \
  --server-port 5001 \
  --benchmark-duration 10 \
  --reconnect-delay 3 \
  --persistent true \
```

### Client Options

| Option | Description | Default |
|--------|-------------|---------|
| --server-ip | Server address | localhost |
| --server-port | Server port | 5001 |
| --benchmark-duration | Benchmark duration (seconds) | 10 |
| --reconnect-delay | Reconnection delay (seconds) | 3 |
| --persistent | Keep trying to reconnect | true |
| --participate | Join processing after upload | false |
| --upload-file | File to upload for processing | none |

## File Processing

### Direct Upload and Processing

```bash
# Upload and process a file
./ffmpeg-cluster-client --upload-file input.mp4

# Upload and participate in processing
./ffmpeg-cluster-client --upload-file input.mp4 --participate true
```

### Server-side Processing

```bash
# Process local file on server
curl -X POST http://localhost:5001/ws/command \
  -H "Content-Type: application/json" \
  -d '{"ProcessLocalFile":{"file_path":"test1.mp4"}}'
```

## Job Management

Jobs progress through the following states:
1. **Queued**: Job is waiting for processing
2. **WaitingForClients**: Waiting for required number of clients
3. **Benchmarking**: Testing client performance
4. **Processing**: Active video processing
5. **Combining**: Merging processed segments
6. **Completed**: Processing finished successfully
7. **Failed**: Processing encountered an error
8. **Cancelled**: Job was manually cancelled

### Job Commands

```bash
# List all jobs
curl http://localhost:5001/ws/command \
  -H "Content-Type: application/json" \
  -d '{"ListJobs":null}'

# Get job status
curl http://localhost:5001/ws/command \
  -H "Content-Type: application/json" \
  -d '{"GetJobStatus":{"job_id":"YOUR_JOB_ID"}}'

# Cancel job
curl http://localhost:5001/ws/command \
  -H "Content-Type: application/json" \
  -d '{"CancelJob":{"job_id":"YOUR_JOB_ID"}}'
```

## Client Management

Clients can be in the following states:
- **Connected**: Initial connection established
- **Benchmarking**: Performance testing
- **Processing**: Active video processing
- **Idle**: Waiting for work
- **Disconnected**: Connection lost

### Client Commands

```bash
# List all clients
curl http://localhost:5001/ws/command \
  -H "Content-Type: application/json" \
  -d '{"ListClients":null}'

# Disconnect client
curl http://localhost:5001/ws/command \
  -H "Content-Type: application/json" \
  -d '{"DisconnectClient":{"client_id":"CLIENT_ID"}}'
```

## Directory Structure

```
work/
├── client/
│   └── {client_id}/
│       ├── benchmark/
│       └── segment_{uuid}/
├── server/
│   ├── uploads/
│   └── {job_id}/
│       └── segments/
└── db/
    ├── client.db
    └── server.db
```

## Performance Considerations

- Processing speed depends on:
  - Available hardware acceleration
  - Network bandwidth
  - Input video characteristics
  - Number of connected clients
  - Client hardware capabilities
  - Video codec and format
  - Segment size distribution

## Error Handling

The system includes comprehensive error handling for:
- Network disconnections
- Processing failures
- Frame count mismatches
- Hardware acceleration issues
- Corrupt video segments
- Database operations
- File system operations

## Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/AmazingFeature`)
3. Commit your changes (`git commit -m 'Add some AmazingFeature'`)
4. Push to the branch (`git push origin feature/AmazingFeature`)
5. Open a Pull Request

## License

This project is licensed under the MIT License - see the LICENSE file for details.