# kdb-tick stack — setup & operation

How to stand up and operate a standard kdb+ tick stack, driven via `q-cli`.
Read this when the user mentions tick, tickerplant, RDB, HDB, or end-of-day.

## The processes

A standard tick stack is three processes:
- **tickerplant (TP):** `q tick.q <schema> <logdir> -p 5010`
- **RDB (real-time DB):** `q tick/r.q :5010 :5012 -p 5011`
- **HDB (historical DB):** `q <hdbpath> -p 5012`

The TP receives updates, logs them, and publishes to subscribers (the RDB). The
RDB holds today's data in memory and writes it down to the HDB at end-of-day.

## Operating it via q-cli

- **Verify each process is up:** `q-cli ping localhost:<port>` /
  `q-cli info localhost:<port>`.
- **Inspect TP subscribers:** `q-cli query localhost:5010 '.u.w'`.
- **RDB row counts:** `q-cli query localhost:5011 'count each tables[]'`.
- **End-of-day:** confirm the `.u.end` / `.Q.dpft` flow before triggering anything
  that writes or clears partitions — this mutates the HDB on disk and is hard to
  undo. Get explicit user confirmation first.

## Cautions

- Treat a tick stack as production unless told otherwise. EOD write-down, partition
  deletes, and `.u.end` are destructive — never trigger them speculatively.
- See [partitioning.md](partitioning.md) for how the HDB stores the written-down
  data (partition domains, `.Q.dpft`, `par.txt` segmentation).
