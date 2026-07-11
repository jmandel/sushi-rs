# Content store

`content_store` is the byte-storage seam shared by compiler, SiteBuild, renderer,
and host code. It deliberately knows nothing about FHIR packages, renderers, or
artifact kinds.

- `ContentRef` carries lowercase SHA-256, exact byte length, and the producer's
  exact optional media type.
- `ContentStore` publishes and retrieves only bytes proven to match a
  `ContentRef`.
- `FileContentStore` uses one regular file per digest. It writes a temporary
  file in the object directory, flushes it, and publishes it with a no-clobber
  rename. Existing content is re-read and verified, never overwritten.
- `VerifiedContent` keeps the exact reference paired with the verified bytes.

Media type is reference metadata, not intrinsic byte identity: the same digest
may correctly be referenced with different media types. Stores therefore
validate that declared media metadata is non-empty and preserve it exactly;
they never infer it from a filename or byte signature.

Native code uses `FileContentStore`. Browser hosts can implement the same
`put`/`read` semantics over OPFS while retaining their asynchronous adapter at
the host boundary.
