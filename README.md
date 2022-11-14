# MyQ Proxy

A local proxy server for interacting with the MyQ cloud service.

## Usage

### CLI

There is a command line interface available in this project for interacting with your account.

Here is the help test:

```log
Usage: myq-cli --config <CONFIG> <COMMAND>

Commands:
  watch        Watch all for all state changes (always begins with add events for each device)
  get-devices  Get a snapshot of the devices and their state
  garage       Set a garage door state
  lamp         Set a lamp state
  help         Print this message or the help of the given subcommand(s)

Options:
  -c, --config <CONFIG>  Path to the config file
  -h, --help             Print help information
  -V, --version          Print version information
```

`get-devices` returns a json blob of all of the devices, you can use the `-t` flag for a smaller
dataset formatted as an ascii table.

### Server

The server expects there to be a configuration file in the global config directory for your platform.
On linux this is `~/.config/myq-server`.

In that direction you'll need to store a json file containing the following.

```json
{
    "Username": "<myq-email-address>",
    "Password": "<myq-password>",
    "state_refresh_s": 60
}

```

The `state_refresh_s` entry is optional, defaulting to 5 seconds

> If you have the option, I recommend creating a guest user on your account for the
> `Username` and `Password` fields here

#### Discovery

The server binds to an ephemeral port, so if you'd like to interact with it it provides discovery
using a UDS socket listening on the multicast address+port `239.255.255.250:1919`, it sends a json
object to any received messages with the shape
`{"target":"myq-proxy", "source": "<named dumped to logs>"}` the response is shaped like this:
`{"ip": "<ip-address>", "port": <port number>}`

#### Interface

##### GET /devices

##### GET /device/{device_id}

##### POST /command/{device_id}

- body: On|Off|Open|Close
- response: Success

##### WS /event-stream

- outgoing messages: see
  [shapshot file](./crates/myq-server/src/snapshots/myq_server__test__outgoing_msg_ser.snap) for all
  options
- incoming messages: see
  [snapshot file](./crates/myq-server/src/snapshots/myq_server__test__outgoing_msg_ser.snap) for all
  options

## Installation

Installation of all assets here requires the rust programming language. You can get that by
visiting [https://rustup.rs](https://rustup.rs)

Once that is available, close this repository

```sh
git clone https://github.com/FreeMasen/myq-proxy
cd ./myq-proxy
```

With that you can build either the cli or server using cargo

```sh
cargo build -p myq-cli --release
cargo build -p myq-server --release
```

Then the binary will be located in `./target/release/`
