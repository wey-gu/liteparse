#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
import re

DOCS_DIR = "./docs/src/content/docs/liteparse"
TMP_DIR = f"{DOCS_DIR}/.api-tmp"

path = f"{TMP_DIR}/README.md"
text = open(path).read()

# Strip implementation and trait implementation sections
text = re.sub(r"#{2,} Implementations\n.*?(?=\n#{2,} )", "", text, flags=re.DOTALL)
text = re.sub(
    r"#{2,} Trait Implementations\n.*?(?=\n#{2,} )", "", text, flags=re.DOTALL
)

# Strip the crate header/version boilerplate
text = re.sub(r"# Crate Documentation\n\n\*\*Version:.*?\*\*Format Version:.*?\n", "", text, flags=re.DOTALL)

# Strip the redundant "Re-exports" section at the end (types are already documented inline)
text = re.sub(r"\n## Re-exports\n.*", "", text, flags=re.DOTALL)

# Strip the top-level "## Modules" line (it's just a bare heading before the actual modules)
text = text.replace("\n## Modules\n", "\n")

# Clean up the "# Module `liteparse`" header to just be a plain intro
text = text.replace("# Module `liteparse`\n", "")

# Remove module grouping headings and their code blocks entirely
text = re.sub(
    r'## Module `\w+`\n\n```rust\npub mod \w+ \{ /\* \.\.\. \*/ \}\n```\n*',
    "",
    text,
)

# Remove bare "### Types" and "### Functions" category headings
text = re.sub(r"### Types\n+", "", text)
text = re.sub(r"### Functions\n+", "", text)

# Promote struct/enum/function headings (####) to ## so they're top-level nav items
# But first, flatten ###### (variant names) to #### so they stay OUT of nav
text = re.sub(r"^######", "####", text, flags=re.MULTILINE)

# Promote #### (Struct/Enum/Function) -> ##
# Promote ##### (Fields/Variants/Methods) -> ###
text = re.sub(r"^#####", "###", text, flags=re.MULTILINE)
text = re.sub(r"^#### (Struct |Enum |Function )", r"## \1", text, flags=re.MULTILINE)

# Remove the "private fields" placeholder table (it's noise for opaque structs)
text = re.sub(
    r"### Fields\n\n\| Name \| Type \| Documentation \|\n\|[-| ]+\|\n\| \*private fields\* \| \.\.\. \| \*Some fields have been omitted\* \|\n+",
    "",
    text,
)

# Collapse verbose enum variant field tables into simple inline type annotations
# Matches pattern: #### `VariantName`\n\nFields:\n\n| Index | Type | ... |\n|---...|\n| 0 | `Type` | |
text = re.sub(
    r"^(#### `\w+`)\n\nFields:\n\n\| Index \| Type \| Documentation \|\n\|[-| ]+\|\n\| 0 \| `([^`]+)` \|  \|",
    r"\1(`\2`)",
    text,
    flags=re.MULTILINE,
)

open(path, "w").write(text)
