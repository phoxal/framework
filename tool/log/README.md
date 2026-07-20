# phoxal-tool-log

The official per-robot structured-log retention tool.

It subscribes to the existing `v1/logs/{participant_id}` feed, preserves the
producer sequence on each record, and assigns a process-wide monotonic ingest
sequence. It retains exactly the newest 1,000 records in memory. A fixed
per-record text budget keeps the complete snapshot inside the bus body ceiling;
additional truncation is disclosed through the existing `truncated` count. The
tool exposes:

- `v1/tool/log/snapshot`: one complete bounded snapshot query;
- `v1/tool/log/follow`: one live record with its resulting cursor.

Each snapshot and follow item also carries `ingest_dropped`, a cumulative
process-local count of samples evicted from tool-log's bounded input ring. It
makes tool-side overload explicitly observable and is separate from each
producer's `Record::dropped`; an increase means the missing source events were
never retained and cannot be recovered by querying.

The participant attribution stored on a record deliberately comes from the
decoded and codec-checked `BusMetadata.source.participant`. The wildcard log
topic is only the subscription selection surface: the typed raw subscriber does
not expose the concrete matched key. The two identities are therefore not
cross-checked, and metadata is the documented authority.

The cursor generation is collision-resistant random data and opaque to
consumers. A consumer installs a snapshot, replays
buffered follow records newer than its cursor, and re-queries whenever the
generation changes or the next sequence is not contiguous.
