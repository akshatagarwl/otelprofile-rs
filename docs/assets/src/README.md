# README assets

The SVG explainers are seeded Rough.js drawings with native SVG animation. They embed the Kalam Latin font, licensed under the SIL Open Font License in `../Kalam-OFL.txt`. Reduced-motion preferences hide the moving dots. The static drawings remain visible.

```sh
cd docs/assets/src
npm ci --ignore-scripts
npm run generate
```

The four PNGs are unedited browser screenshots of the checkout demo running against sigiro `3c801fb`, built separately from its source. CPU and heap screenshots show real ingested data. The trace screenshot opens a real exported trace; the span screenshot follows its actual **Open profile** link and verifies a span-filtered URL. No sigiro source changes or mocked browser responses are used.

Reproduce with Google Chrome installed, sigiro serving its embedded dashboard at `http://localhost:19998`, and its OTLP HTTP receiver on `14338`:

```sh
# From the repository root. Debug info keeps application function names visible.
CARGO_PROFILE_RELEASE_DEBUG=1 cargo build --release -p otelprofile-demo
AFTER_MS=$(($(date +%s) * 1000))
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:14338 target/release/otelprofile-demo
cd docs/assets/src
AFTER_MS="$AFTER_MS" npm run screenshots
```

`SIGIRO_URL` changes the dashboard/query URL. `AFTER_MS` limits the source data to a fresh demo run; without it, the script uses all checkout data. Query results, not hardcoded trace IDs, choose the screenshots. Browser errors, failed responses, missing data, and missing span-profile navigation fail the script. The original capture used a 1440-pixel viewport and full-page screenshots on 2026-11-09.

After pushing, run `npm run check-readme` to open the actual GitHub README. It requires all seven images to load at readable intrinsic sizes and verifies that two captures of the flow image differ while the animation runs. This caught missing SVG width/height attributes that made GitHub render the diagrams only 300 pixels wide.
