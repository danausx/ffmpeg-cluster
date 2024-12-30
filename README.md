# FFmpeg Cluster: Distributed Video Processing

## Overview

FFmpeg Cluster is a distributed video processing based in Rust and FFmpeg. It leverages multiple clients to process and encode video files in parallel. By splitting video processing across multiple machines, this project aims to significantly reduce encoding time and maximize computational resources on clustered encoding setups.

## Key Features

- **Distributed Processing**: Dynamically splits video processing across multiple clients
- **Flexible Configuration**: Customizable FFmpeg encoding parameters
- **Websocket-based Communication**: Real-time coordination between server and clients
- **Automatic Segment Management**: Intelligent frame distribution and segment recombination
- **Robust Error Handling**: Comprehensive error tracking and recovery

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
- FFmpeg installed
- Cargo package manager

## Installation

1. Clone the repository:
   ```bash
   git clone https://github.com/yourusername/ffmpeg-cluster.git
   cd ffmpeg-cluster
   ```

2. Build the project:
   ```bash
   cargo build --release
   ```

## Usage

### Server

Start the server with custom parameters:
```bash
cargo run --bin ffmpeg-cluster-server -- \
  --file-name input.mp4 \
  --required-clients 3 \
  --port 5001
```

### Client

Connect a client to the server:
```bash
cargo run --bin ffmpeg-cluster-client -- \
  --server-ip localhost \
  --server-port 5001
```

## Configuration Options

### Server Configuration
- `--required-clients`: Number of clients needed to start processing
- `--file-name`: Input video file
- `--file-name-output`: Output video file name
- `--ffmpeg-params`: Custom FFmpeg encoding parameters

### Client Configuration
- `--server-ip`: Server IP address
- `--server-port`: Server port
- `--benchmark-duration`: Benchmark processing duration

## Example Workflow

1. Start the server with an input video
2. Launch multiple clients
3. Clients connect and perform benchmarks
4. Video is split and processed in parallel
5. Segments are recombined into final output

## Performance Considerations

- Processing speed depends on:
  - Number of clients
  - Client hardware capabilities
  - Video characteristics
  - Encoding parameters

## Roadmap

- [ ] Add more extensive logging
- [ ] Implement advanced error recovery
- [ ] Support for more video processing scenarios
- [ ] Job handling and resume features
- [ ] Enhanced client load balancing
- [ ] Comprehensive web UI
- [ ] Live streams comptaibility
- [ ] Jellyfin compatibility

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/AmazingFeature`)
3. Commit your changes (`git commit -m 'Add some AmazingFeature'`)
4. Push to the branch (`git push origin feature/AmazingFeature`)
5. Open a Pull Request