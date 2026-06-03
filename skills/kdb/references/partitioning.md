# Partitioning data in kdb+

Reference for partitioned historical databases (HDB): partition domains, when to
use date vs int (time/hour) partitioning, on-disk layout, save/query patterns, and
caveats. Based on the kdb+ data model and KX's "Partitioning data in kdb+" guide.

## The four partition domains

A partitioned table is split into sub-directories of the HDB, one per partition
value. The **partition column is virtual** — it is *not* stored in the data; q
derives it from the directory name when the DB is loaded. Only four column
types may be the partition domain:

| domain  | on-disk directory | typical use                                   |
|---------|-------------------|-----------------------------------------------|
| `date`  | `2020.06.25`      | **most common** — one partition per day        |
| `month` | `2020.06`         | lower-frequency / very long histories          |
| `year`  | `2020`            | reference / slow-changing data                 |
| `int`   | `179608`          | anything keyed by an integer — **hour**, size-based buckets, custom schemes |

```
/hdb
  /2020.06.24
    /trade/   (col files: sym, time, price, size, ...)
    /quote/
  /2020.06.25
    /trade/
    /quote/
  sym            # the enumeration domain (symbols), shared across partitions
```

Int layout is identical but the directories are plain integers:

```
/hdb
  /179608
    /trade/
    /quote/
  /179609
    ...
```

## date partitioning (the default)

One directory per calendar day. After loading, every partitioned table gains a
virtual `date` column. **Always constrain the partition column first** in queries
so q only opens the relevant directories instead of scanning the whole DB:

```q
select vwap:size wavg price by sym from trade
  where date=2020.06.25, sym in `AAPL`MSFT
```

q runs cross-partition queries with **map-reduce across slave threads** (`-s`),
so date-range aggregations parallelize naturally.

## int partitioning — time/hour and size-based

When daily partitions are too coarse (very high volume, or you want to flush RAM
intraday), use `int` partitioning. The integer can encode anything; two common
schemes:

**Hourly** — partition number = hours since the kdb+ epoch (2000.01.01):

```q
hour:{`int$sum 24 1*`date`hh$\:x}   / timestamp -> hours-since-epoch int
hour 2020.06.25D13:04:00.000        / -> 179653
```

A virtual `int` column (e.g. these hour buckets) replaces the `date` column.
Queries constrain on it the same way:

```q
select from trade where int=hour[2020.06.25D13:00:00], sym=`AAPL
```

**Fixed-size** — start a new partition when the tickerplant log passes a size
threshold (`hcount`), regardless of elapsed time. Keeps each partition's RAM
footprint bounded:

```q
if[n <= hcount L; endofpart[]]     / roll the partition when log exceeds n bytes
```

For fixed-size schemes, keep a **lookup table** mapping each partition number to
its timestamp range, so a query on a time window can pick the right partitions
without scanning every one.

## date vs int — choosing

- **date** — standard daily capture; simplest; best default. Use unless a day's
  data is too large to write down or hold in the RDB.
- **int (hourly / size-based)** — reduces RAM by flushing intraday, gives
  partition sizing independent of the calendar, and tames very-high-volume feeds.
  Hourly is "simple to implement but not as powerful as a fully thought-out
  intraday-writedown solution."

> Don't over-engineer the trigger: "There is little benefit to attempting to be
> too exact… choosing a simple method and allowing a cautious RAM overhead is the
> best path."

## Saving a partition: `.Q.dpft`

`.Q.dpft[dir; part; field; table]` writes an in-memory table to the HDB as a
partition, applying the **parted attribute** (`` p# ``) on `field` (usually `sym`)
so `where sym=` lookups are fast. Returns the table name on success.

```q
.Q.dpft[`:/hdb; 2020.06.25; `sym; `trade]   / write today's trade, parted on sym
```

`.Q.dpfts` is the variant that also writes the enumeration (sym) file. End-of-day
in a tick stack: the RDB sorts each table by `sym`, calls `.Q.dpft`, then empties
the in-memory tables.

## Useful introspection (run via q-cli)

| expr | meaning |
|------|---------|
| `.Q.pf` | partition field — `` `date ``/`` `month ``/`` `year ``/`` `int `` |
| `.Q.pv` | partition values present (e.g. list of dates) |
| `.Q.PV` | partition values aligned to the int domain |
| `.Q.pt` | list of partitioned tables |
| `.Q.qp x` | is `x` partitioned (`1b`) / splayed (`0b`) / neither |
| `.Q.par[dir;part;tbl]` | resolve the on-disk path of a partition |

```sh
q-cli query @hdb '.Q.pf'                 # which domain is this HDB partitioned on?
q-cli query @hdb '.Q.pv'                 # what partitions exist?
q-cli query @hdb 'count each .Q.pt!...'  # tables per partition
```

## Segmentation: `par.txt`

To spread partitions across multiple disks, put a `par.txt` in `QHOME`/HDB root
listing the storage directories, one per line. kdb+ round-robins partitions
across them and presents one logical DB. `bin` finds the prevailing partition for
a value when locating data across segments.

## Caveats / best practices

- **Virtual column is not stored** — never add a `date`/`int` column to the data
  itself; it comes from the directory name. Selecting it works; storing it wastes
  space and can conflict.
- **Parted attribute** — keep `` p# `` on `sym` (data sorted by sym within a
  partition) for fast symbol lookups; `.Q.dpft` applies it for you.
- **Constrain the partition column first** — `where date=…` (or `where int=…`)
  before other predicates, or q scans every partition.
- **Late data** — with multiple timestamp columns, a row may belong to the prior
  bucket; buffer near boundaries, e.g. `buffInts:{hour 0D00:01+x}`.
- **File / inode proliferation** — int (esp. hourly) partitioning creates many
  small directories; watch `ulimit`/inode limits and run a **defrag** process to
  consolidate column files after write-down.
- **Symbols are enumerated** against the shared `sym` file across all partitions —
  don't delete it.

## Sources
- KX — Partitioning data in kdb+: https://kx.com/blog/partitioning-data-in-kdb/
- q for Mortals §14 (Introduction to Kdb+) — partitioned tables & partition domain.
- KX docs — intraday write-down: https://code.kx.com/q/wp/intraday-writedown
