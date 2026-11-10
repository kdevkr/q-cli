# q-cli Usage Examples

Comprehensive examples for querying, scripting, formatting, and inspecting
kdb+ databases with `q-cli`.

## Basic Queries

```sh
# Connect via explicit host:port
q-cli query localhost:5000 "1+1"
q-cli query localhost:5000 "select avg price by sym from trade"

# Connect via configured server profile (@profile or @ for default)
q-cli query @ "select count i by sym from trade"
q-cli query @prod "select from trade where price > 100"

# Read multi-line query from stdin
cat report.q | q-cli query @ -
echo "select max price from trade" | q-cli query @ -
```

## Output Modes

### 1. Formatted Text (Default)
```sh
q-cli query @ "select from trade"
# Limit display rows (default: 50; 0 = unlimited)
q-cli --max-rows 10 query @ "select from trade"
```

### 2. JSON
Produces a JSON array of row objects (or primitive values).
```sh
q-cli --json query @ "select sym, price from trade"
# Keyed tables automatically merge key and value columns
q-cli -j query @ "2!select by sym from trade"
```

### 3. CSV
Uncapped RFC 4180 compliant CSV export for tables.
```sh
q-cli --csv query @ "select from trade" > trade.csv
```

### 4. Console (.Q.s)
Lets the remote q process format the output directly for max fidelity.
```sh
q-cli --console query @ "flip 0!exec sym from trade"
```

## Schema & Diagnostics

```sh
# Connection probe
q-cli ping @prod

# List all tables and views
q-cli tables @prod
q-cli views @prod

# Inspect column schemas and row counts
q-cli meta @prod trade
q-cli count @prod trade

# Full table overview in 1 round-trip: partitions, schema, row count & sample
q-cli describe @prod trade

# Full database overview in 1 round-trip: all tables with rows/cols/partitions
q-cli schema @prod

# Functions defined in root or specific namespace with arities
q-cli functions @prod
q-cli functions @prod .Q

# Server health snapshot (version, pid, port, memory, open handles)
q-cli info @prod

# Query execution benchmark (elapsed ms + row count, data omitted)
q-cli time @prod "select avg price by sym from trade"

# Free server memory back to the operating system
q-cli gc @prod
```

## Running `.q` Scripts

```sh
# Execute a script file statement by statement (reports line number on error)
q-cli run @ init.q

# Read script from stdin
cat migration.q | q-cli run @ -

# Validate syntax on server without executing or mutating state
q-cli --dry-run run @ script.q

# Execute as a single atomic protected block
q-cli --atomic run @ transaction.q

# Isolate local variables in a lambda so globals are not polluted
q-cli --scoped run @ script.q
```

## Server Profiles (`config`)

`q-cli` supports two configuration layers: global (`~/.config/q-cli/servers.conf`)
and project-local (`./.q-cli.conf`).

```sh
# Initialize global config file
q-cli config init

# Add or update server profiles (passwords are masked in output and listings)
q-cli config add dev localhost:5000
q-cli config add prod bigbox:5001:alice:secret

# Add project-local profile (creates ./.q-cli.conf in current directory)
q-cli config add --project test [::1]:5002

# View merged configuration (tagged with [global] and [project])
q-cli config list

# Show paths to active config files
q-cli config path
```

## Runtime Tuning (`web` & `trace`)

```sh
# Control built-in HTTP server on the q port
q-cli web status @
q-cli web off @         # Reject all incoming HTTP requests
q-cli web get-ok @      # Allow HTTP GET (200 OK), reject POST

# Wrap server handlers with .Q.trp for detailed error backtraces
q-cli trace on @
q-cli trace status @
q-cli trace off @
```

## Self-Update (`update`)

Checks GitHub Releases for `kdevkr/q-cli`, downloads the matching pre-built binary,
and replaces the running executable in-place without compilation. Downloads are
HTTPS-only (redirects included), restricted to this repository's release URLs, and
verified against GitHub's SHA-256 asset digest before the binary is replaced; an
asset without a digest is never installed.

```sh
# Check if an update is available without downloading
q-cli update --check

# Structured JSON output for CI / agent automation
q-cli --json update --check

# Self-update to the latest release
q-cli update

# Force reinstall / update even if already on the latest version
q-cli update --force
```
