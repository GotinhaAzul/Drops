"""Selections/document identity through the real capture pipeline."""
import dataclasses
import unittest
from types import SimpleNamespace

from test_capture_text import FakeBackend, FakeNode, make_app, make_frame, make_window
import capture_text


@dataclasses.dataclass
class AppNode(FakeNode):
    uri: str = ""
    selections: list = dataclasses.field(default_factory=list)
    selected: list = dataclasses.field(default_factory=list)


class AppBackend(FakeBackend):
    def document_uri(self, node):
        return getattr(node, "uri", "")

    def text_selections(self, node, limit):
        return [{"text":text[:limit], "start":0, "end":len(text), "truncated":len(text)>limit}
                for text in getattr(node, "selections", [])]

    def selected_children(self, node):
        children=getattr(node, "selected", [])
        return children[:20], len(children)


class AppContextTests(unittest.TestCase):
    def capture(self, nodes, kind="browser", **extra):
        context=make_window(42, "Target")
        context.update(app_kind=kind, **extra)
        backend=AppBackend([make_app(42,[make_frame("Target",nodes,42)])])
        return capture_text.run_capture(context, backend=backend, enable_tmux=False)

    def test_page_uri_and_highlighted_text_are_distinct_from_page_body(self):
        node=AppNode(role="document frame",name="Example page",uri="https://example.com/article?q=1",states={"showing"},text="Whole page body",selections=["Chosen sentence"])
        result=self.capture([node])
        self.assertEqual(result["app_context"]["documents"][0]["uri"],node.uri)
        self.assertEqual(result["app_context"]["selections"][0]["text"],"Chosen sentence")
        self.assertIn("Whole page body",result["sources"][0]["text"])

    def test_no_selection_is_inferred_from_ordinary_text_or_title(self):
        node=AppNode(role="document frame",name="file:///tmp/pretend",text="Selected: invented",states={"showing"})
        app=self.capture([node])["app_context"]
        self.assertEqual(app["selections"],[])
        self.assertEqual(app["documents"],[])

    def test_hidden_tabs_and_password_selections_are_never_captured(self):
        hidden=AppNode(role="document frame",uri="https://hidden.example",states={"visible"},text="hidden body",selections=["hidden selection"])
        password=AppNode(role="password text",states={"showing"},text="test secret",selections=["test secret"])
        visible=AppNode(role="text",states={"showing"},text="safe",selections=["safe"])
        result=self.capture([hidden,password,visible])
        self.assertEqual(result["app_context"]["selections"][0]["text"],"safe")
        self.assertNotIn("hidden body",str(result))
        self.assertNotIn("test secret",str(result))

    def test_metadata_probe_does_not_read_body_text(self):
        node=AppNode(role="text",states={"showing"},text="Do not read whole body",selections=["Just selection"])
        result=self.capture([node],app_probe=True)
        self.assertEqual(result["sources"][0]["text"],"")
        self.assertNotIn("Do not read whole body",str(result))
        self.assertEqual(result["app_context"]["selections"][0]["text"],"Just selection")

    def test_selection_after_preview_budget_is_still_found_and_bounded(self):
        nodes=[AppNode(role="text",states={"showing"},text="x"*100),AppNode(role="text",states={"showing"},selections=["s"*10000])]
        result=self.capture(nodes,capture_options={"max_text_chars":10})
        selection=result["app_context"]["selections"][0]
        self.assertEqual(len(selection["text"]),6000)
        self.assertTrue(selection["truncated"])

    def test_selected_explorer_items_use_selection_interface(self):
        file=AppNode(name="report.txt",role="table cell",states={"showing"},attrs={"uri":"file:///tmp/report.txt"})
        container=AppNode(role="table",states={"showing"},selected=[file],children=[file])
        result=self.capture([container],kind="file_manager")
        self.assertEqual(result["app_context"]["selected_items"],[{"name":"report.txt","uri":"file:///tmp/report.txt","container_role":"table"}])
        self.assertEqual(self.capture([container],kind="browser")["app_context"]["selected_items"],[])

    def test_disabled_accessibility_does_not_query_metadata(self):
        result=self.capture([AppNode(role="text",states={"showing"},selections=["not captured"])],capture_options={"accessibility":False})
        self.assertEqual(result["app_context"],{})

    def test_gi_adapter_reads_only_the_selected_range(self):
        calls=[]
        text=SimpleNamespace(get_n_selections=lambda node:1,
            get_selection=lambda node,index:SimpleNamespace(start_offset=100,end_offset=1000),
            get_text=lambda node,start,end:calls.append((start,end)) or "x"*(end-start))
        backend=capture_text.AtspiBackend(SimpleNamespace(Text=text))
        result=backend.text_selections(SimpleNamespace(is_text=lambda:True),50)
        self.assertEqual(calls,[(100,150)])
        self.assertTrue(result[0]["truncated"])


if __name__=="__main__":
    unittest.main()
