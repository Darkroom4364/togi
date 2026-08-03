# Crates.io Publishing Policy

This policy governs publication of the `togi` CLI crate to crates.io. GitHub release
archives remain the tested binary-install path described in the README; a crates.io
release must identify the same source package as its corresponding Git tag.

## Preconditions

A maintainer may publish version `X.Y.Z` only when all of these conditions are
recorded in the tracking issue or its merged release-record PR:

1. The release tag `vX.Y.Z` is protected against moves and deletions. The clean
   source checkout uses its peeled commit, which is recorded and matches both
   the GitHub release target and the successful release-workflow head. Recheck
   all three immediately before upload and after publication. Its
   [`Cargo.toml`](../Cargo.toml) declares exactly `X.Y.Z`.
2. The matching GitHub release is published, not a draft or prerelease, and its
   release workflow completed successfully.
3. The current [compatibility contract](COMPATIBILITY.md) applies to that
   release, and its documented release evidence is available.
4. Crate-name availability or ownership is checked against crates.io at the
   decision time. An unrelated existing owner rejects publication.
5. A current `cargo publish --dry-run --locked` succeeds from the tagged
   checkout. Record its timestamp, command/output, package summary, and peeled
   commit; repeat and record that final evidence immediately before upload.
6. A maintainer records an explicit approval after reviewing the preceding
   evidence.

## Procedure

1. Verify release-tag protection, then resolve and record the tag's peeled
   commit, the GitHub release target, and the successful release-workflow head.
   Stop on any mismatch.
2. At the decision time, record the registry-name result and the dry-run
   evidence in the tracking issue.
3. Immediately before upload, re-resolve the three source identities and repeat
   the dry-run from the clean tagged checkout. Record that final timestamp,
   command/output, package summary, and peeled commit in the tracking issue
   before `cargo publish`.
4. Publish from that checkout with `cargo publish --locked`; never publish a
   package whose source has moved past its release tag.
5. After Cargo accepts the upload, re-resolve and record the three source
   identities. Any change is a release-integrity failure: do not update public
   installation guidance until it is investigated and recorded.
6. Record the crates.io URL and version returned by Cargo, then update public
   installation guidance in a reviewed PR.

If any precondition fails, record the failure and do not publish. A later release
uses a fresh decision rather than reusing a stale name-availability or dry-run
check.
