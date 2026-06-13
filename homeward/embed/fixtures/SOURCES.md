# Fixture Image Sources

This directory contains sample pet images used by `homeward-embed smoke` for
end-to-end validation of the enroll→index→query pipeline.

## Production fixtures (real photos)

The following permissively-licensed photos are the intended production fixtures.
They are **not committed to this repo** (Git LFS or direct download required)
because their file sizes exceed what belongs in source control.  Run
`scripts/fetch-fixtures.sh` (or see the URLs below) to populate this directory.

| Filename | Subject | License | Source URL |
|---|---|---|---|
| `dog_retriever.jpg` | Golden Retriever on grass | CC BY 2.0 | https://commons.wikimedia.org/wiki/File:Golden_Retriever_Angus.jpg |
| `dog_dalmatian.jpg` | Dalmatian portrait | CC BY-SA 2.0 | https://commons.wikimedia.org/wiki/File:Dalmatiner.jpg |
| `cat_tabby.jpg` | Tabby cat sitting | CC BY 2.0 | https://commons.wikimedia.org/wiki/File:Cat_sitting_on_a_fence.jpg |

All images are from Wikimedia Commons and used under their respective Creative
Commons licenses.  No copyright-encumbered images are included.

## Synthetic fallback

When the real JPEG fixtures are absent, `homeward_embed.cli` auto-generates
solid-colour synthetic PNG images (one per "individual") in a temporary
directory.  These are sufficient to verify the rank-ordering assertion because
DINOv2 encodes colour statistics in the CLS token — visually identical images
will produce nearly identical vectors, while differently-coloured images will
produce distinct vectors.

The synthetic fallback is **only for smoke/CI validation** and carries no
accuracy claim on real shelter data (that is `homeward-eval-harness` territory).
