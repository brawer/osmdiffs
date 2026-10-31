# Output files

This directory documents the files this project produces for public
consumption — what’s in them, and how to read them. (For how the
pipeline itself is built and tested, see the parent [`docs/`](../)
directory instead — those pages are for people working on the
pipeline’s code, not for people using its output.)

- [`CONFLATED_PARQUET.md`](CONFLATED_PARQUET.md) — `conflated.parquet`,
  pairing AllThePlaces features with their matching OpenStreetMap
  features.
- [`CONFLATED_TILES.md`](CONFLATED_TILES.md) — `conflated.pmtiles`,
  visualizing `conflated.parquet`, matched or not, useful for
  debugging.

## Downloading the current output

The pipeline isn’t running in production yet (see
[`docs/TECHNICAL_DESIGN.md`](../TECHNICAL_DESIGN.md#status)), so nothing
is being published on a schedule yet. But this is only a matter of
scheduling; the update mechanism is already in place.

Discovery goes through one small file, a
[Frictionless Data Package](https://datapackage.org/) descriptor at
**`https://osmdiffs.brawer.ch/data/datapackage.json`**. Fetch it (~1 KB), read
`version` (the release date) to check for updates, and resolve each
`resources[].path` — a bare, dated filename — against the descriptor’s
own URL to get the actual download link. Every file except the
descriptor is immutable: its name carries a date and a content hash, and
`resources[].bytes` / `resources[].hash` (`sha256:…`) let you verify it.

```sh
host=https://osmdiffs.brawer.ch
curl -s "$host/data/datapackage.json" \
  | jq -r --arg h "$host" '.resources[] | "\($h)/data/\(.path)  \(.name)"'
```

On Windows, PowerShell can do the same without `jq`, since
`Invoke-RestMethod` parses the JSON for you:

```powershell
$base = 'https://osmdiffs.brawer.ch/data'
$pkg = Invoke-RestMethod "$base/datapackage.json"
$pkg.resources | ForEach-Object { "$base/$($_.path)  $($_.name)" }
```

To download a resource and check its hash, use `curl.exe` (part of
Windows 10 and later; write `curl.exe`, not `curl`, because in Windows
PowerShell `curl` is an alias for `Invoke-WebRequest`, which is much
slower on large files):

```powershell
$r = $pkg.resources | Where-Object name -eq 'conflated'
curl.exe -fL -o $r.path "$base/$($r.path)"
$hash = (Get-FileHash $r.path -Algorithm SHA256).Hash.ToLower()
if ("sha256:$hash" -ne $r.hash) { Write-Error "hash mismatch: $($r.path)" }
```

The `sh` example above also works as is under
[WSL](https://learn.microsoft.com/windows/wsl/) or Git Bash, if you
have `jq` installed there.

The `conflated` resource is [`conflated.parquet`](CONFLATED_PARQUET.md);
`bom` is its [CycloneDX](CONFLATED_PARQUET.md#data-provenance)
provenance BOM as a standalone file. The two `*-tiles` resources are
PMTiles archives you can open right in the browser via
[`pmtiles.io`](https://pmtiles.io) (append
`#url=<the-url>&inspectFeatures=true`) — but they’re a debugging aid,
not a data product; build on `conflated.parquet` instead.
