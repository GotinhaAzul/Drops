#!/usr/bin/env python3
"""Unit checks for ui/markdown.js: Markdown and LaTeX to safe rich text."""
import json
import pathlib
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
MODULE = ROOT / "ui/markdown.js"


def render(cases):
    node = shutil.which("node")
    if not node:
        return None
    script = (
        "const md=require(process.argv[1]);"
        "const cases=JSON.parse(process.argv[2]);"
        "console.log(JSON.stringify(cases.map(text=>md.render(text))));"
    )
    result = subprocess.run(
        [node, "-e", script, str(MODULE), json.dumps(cases)],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


def main():
    cases = {
        "headings": "# One\n## Two\n### Three",
        "emphasis": "**bold** *italic* ***both*** ~~gone~~ `code`",
        "lists": "- one\n- two\n  1. nested\n  2. second\n- three\n\n- [x] done\n- [ ] todo",
        "quote_fence": "> quoted **text**\n\n```js\nconst x = **not bold**;\n```",
        "table": "| A | B |\n|:---|---:|\n| 1 | 2 |",
        "link": "[real](https://example.com/a_(b)) and <https://x.test>",
        "unsafe_link": "[bad](javascript:alert(1))",
        "html": "<b>raw</b> & <script>x</script>",
        "inline_math": "$E=mc^2$ and $\\alpha + \\beta$",
        "fraction_sqrt_sum": "$\\frac{a+b}{c} + \\sqrt{x} + \\sum_{i=1}^{n} i$",
        "display_math": "$$\\int_0^1 x^2\\,dx$$",
        "matrix": "$$\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}$$",
        "cases": "$$\\begin{cases} x & \\text{if } a \\\\ y & \\text{otherwise} \\end{cases}$$",
        "aligned": "$$\\begin{aligned} x &= 1 \\\\ y &= 2 \\end{aligned}$$",
        "advanced": "$\\sqrt[3]{x} + \\binom{n}{k} + x \\pmod{n}$",
        "text": "$\\text{hello world} + 1$",
        "blackboard": "$\\mathbb{R}^n$ and $\\mathcal{L}$",
        "currency": "Cost $10 and $20.",
        "escapes": "\\*not italic\\* and **bold**",
        "line_break": "one\\\ntwo",
        "code_literal": "`<b>**not**</b>`",
        "unknown": "$\\foobar{x}$",
    }
    out = render(list(cases.values()))
    if out is None:
        print("SKIP: node is not installed; run the QML renderer checks instead")
        return
    result = dict(zip(cases.keys(), out))

    assert "<font size=\"+2\">One</font>" in result["headings"]
    assert "<font size=\"+1\">Two</font>" in result["headings"]
    assert "<b><i>both</i></b>" in result["emphasis"]
    assert "<s>gone</s>" in result["emphasis"]
    assert "font-family:monospace" in result["emphasis"]

    lists = result["lists"]
    assert "<ol start=\"1\"" in lists and "<ul" in lists
    assert "nested" in lists and "☑" in lists and "☐" in lists
    assert "<blockquote" in result["quote_fence"]
    assert "<pre" in result["quote_fence"]
    assert "**not bold**" in result["quote_fence"]
    assert "<table" in result["table"] and "<th" in result["table"]
    assert "<a href=\"https://example.com/a_(b)\"" in result["link"]
    assert "<a href=\"https://x.test\"" in result["link"]

    assert "href" not in result["unsafe_link"]
    assert "bad" in result["unsafe_link"]
    assert "&lt;script&gt;" in result["html"]
    assert "<script>" not in result["html"]

    assert "E=mc²" in result["inline_math"]
    assert "α + β" in result["inline_math"]
    frac = result["fraction_sqrt_sum"]
    assert "(a+b)/c" in frac and "√(x)" in frac and "∑ᵢ₌₁ⁿ" in frac
    assert "align=\"center\"" in result["display_math"]
    assert "∫₀¹ x² dx" in result["display_math"]
    assert "(a  b<br/>c  d)" in result["matrix"]
    assert "if" in result["cases"] and "otherwise" in result["cases"]
    assert "<br/>" in result["aligned"] and "x" in result["aligned"] and "y" in result["aligned"]
    assert "∛(x)" in result["advanced"] and "C(n, k)" in result["advanced"]
    assert "(mod n)" in result["advanced"]
    assert "hello world + 1" in result["text"]
    assert "ℝⁿ" in result["blackboard"] and "ℒ" in result["blackboard"]

    assert "$10" in result["currency"] and "$20" in result["currency"]
    assert "<i>" not in result["escapes"] and "not italic" in result["escapes"]
    assert "<b>bold</b>" in result["escapes"]
    assert "<br/>" in result["line_break"]
    assert "&lt;b&gt;**not**&lt;/b&gt;" in result["code_literal"]
    assert "\\foobar" in result["unknown"]

    print(f"PASS: {len(cases)} Markdown/LaTeX renderer cases")


if __name__ == "__main__":
    main()
