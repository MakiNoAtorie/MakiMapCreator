# Seeds are stable within one app version only

A seed and parameters give the same result within one app version. A later version may give a different map for the same seed. Saved projects do not change, because generated output is stored as data (ADR-0003). The project file records the app version that created each generation. We rejected cross-version stability because it would freeze the generators.
