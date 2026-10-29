# org-membership

Port of `bench/rls_bench.exs`. Documents belong to orgs; users are members of orgs; comments are
visible iff their document is. Also a 16-way hash-partitioned copy of documents.

Subjects: `selective` (user 1, ~2 orgs) and `broad` (user 0, half of all orgs).
Scale = number of documents (default 50k; the original used 1M).

Expected symptom: per-row function (naive_rls) is 100-100,000x slower than the explicit semi-join;
the InitPlan-array form is still slow on ORDER BY LIMIT, nested policies and partitions (no join
reordering, no partition pruning).
