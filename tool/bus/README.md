# phoxal-tool-bus

The official per-robot bus-rate observation and retention tool.

It mirrors this robot's versioned contract traffic into bounded topic-producer
counters. Every second it completes one rate window, retains exactly the newest
60 completed windows in memory, and exposes:

- `v1/tool/bus/snapshot`: the current partial counters plus the complete bounded
  completed-window history;
- `v1/tool/bus/follow`: one newly completed window with its resulting cursor.

The cursor generation is collision-resistant random data and opaque to
consumers. A consumer installs a snapshot, replays
buffered follow windows newer than its cursor, and re-queries whenever the
generation changes or the next sequence is not contiguous.
