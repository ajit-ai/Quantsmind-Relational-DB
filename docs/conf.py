# QuantsMind docs — Sphinx configuration (RST -> HTML for GitHub Pages).
import os
import sys

sys.path.insert(0, os.path.abspath("."))

project = "QuantsMind Relational Database Engine"
copyright = "2026, QuantsMind"
author = "QuantsMind"

# The short X.Y version and the full release string.
version = "0.1.0"
release = "0.1.0"

extensions = ["sphinx_rtd_theme"]

templates_path = ["_templates"]
exclude_patterns = ["_build", "Thumbs.db", ".DS_Store"]

# -- Options for HTML output -------------------------------------------------

html_theme = "sphinx_rtd_theme"
html_title = "QuantsMind — Relational Database Engine"
html_logo = None
html_favicon = None
html_static_path = ["_static"]