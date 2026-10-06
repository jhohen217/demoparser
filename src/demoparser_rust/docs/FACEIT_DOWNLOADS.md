# FACEIT downloads

The `demofetch` Rust library is compiled into `Demoparser`; no separate downloader executable
or Python installation is required at runtime. Run `Demoparser fetch --help` for current options.

## Local credentials

Create an untracked `faceit.ini` beside the executable, or supply `--credentials <ini>`:

```ini
[FACEIT]
DataApiKey =
DownloadToken =
DownloadDirectory = demos
Concurrency = 3
```

Supply your own credentials with the required FACEIT Data and Downloads API access.
`FACEIT_API_KEY` and `FACEIT_DOWNLOAD_TOKEN` environment variables override the INI credentials.
The downloader also checks executable- and working-directory `config.ini` files. No populated
credentials are distributed. `--config <ini>` selects the parser configuration independently.

## Examples

```text
Demoparser fetch <match-id-or-room-url> --output demos
Demoparser fetch --matches-file ace_matches.txt --mode trim --output demos
Demoparser fetch --nickname PLAYER --limit 20 --mode parse --output demos
Demoparser fetch --nickname PLAYER --oldest-available --discover-only
Demoparser --capabilities
```

`download` saves validated archives; `trim` catalogs configured collections and writes verified
round clips; `parse` also generates configured replay/tick assets. Parser jobs are serialized
while downloads may run concurrently. Source deletion requires explicit configuration and
successful verified publication. Partial selection runs retain originals.

Match queues accept UUIDs, FACEIT room links and supported legacy rows. Batch queue files must
use `ace.txt`, `ace_*.txt` or `ace-*.txt` names. A source-list name does not certify kill counts.
Multiple demo resources in one match are handled. Valid existing files are reused; corrupt
existing files are retained and reported. Transfers validate raw/gzip/Zstandard inputs before
publication. Ctrl+C cooperatively cancels work and retains completed files.

The adaptive availability search samples real resources; age alone does not establish expiry.
Only explicit unavailable responses qualify queue entries for `--purge-expired`. Network,
permission and inconclusive responses must not remove entries. Purging modifies the supplied
queue, so use it only when that is intended. `--discover-only` saves findings without full downloads.

Use `--report-directory` to select the JSON report location and `--ndjson` for host integration.
Only `fetch_finished` ends an entire fetch job; an intermediate parser `finished` event ends
one parser job. Hosts should accept complete JSON objects and ignore ordinary diagnostic lines.
Reports contain paths, hashes and errors, without API credentials or signed resource URLs.

Live credential/network access is not part of the publication-candidate smoke tests. See the
current [FACEIT API documentation](https://docs.faceit.com/) for obtaining API access.
