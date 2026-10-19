// ExternalInterface probe (AS3 via Haxe): registers callbacks, calls a page function, reports through navigateToURL.
import flash.display.*;
import flash.events.*;
import flash.net.*;
import flash.external.ExternalInterface;
import flash.utils.Timer;
import flash.text.*;

class EI extends Sprite {
	var results:Array<String> = [];
	var sent = false;
	var tf:TextField;
	static function main() { flash.Lib.current.addChild(new EI()); }
	function new() {
		super();
		addEventListener(Event.ADDED_TO_STAGE, init);
	}
	function report(k:String, v:Dynamic) { results.push(k + "=" + Std.string(v)); if (tf != null) tf.text = results.join("\n"); }
	function init(_) {
		tf = new TextField(); tf.defaultTextFormat = new TextFormat("_sans", 11, 0x202020); tf.x = 4; tf.y = 4; tf.width = 390; tf.height = 112; tf.multiline = true; tf.wordWrap = true; addChild(tf);
		report("available", ExternalInterface.available);
		report("objectID", ExternalInterface.objectID);
		if (ExternalInterface.available) {
			try {
				ExternalInterface.addCallback("echo", function(s:Dynamic):Dynamic { report("echo_in", s); return "flash:" + Std.string(s); });
				ExternalInterface.addCallback("sum", function(a:Float, b:Float):Float { return a + b; });
				ExternalInterface.addCallback("obj", function(o:Dynamic):Dynamic { report("obj_in", haxe.Json.stringify(o)); return { n: 1, arr: ([1, "two", true, null] : Array<Dynamic>), nested: { k: "v" } }; });
				report("addCallback", "ok");
			} catch (e:Dynamic) { report("addCallback", "ERR " + Std.string(e)); }
			try {
				var r:Dynamic = ExternalInterface.call("pageEcho", "hello", 42, true, { a: 1, b: ([1, 2] : Array<Dynamic>) });
				report("call_pageEcho", haxe.Json.stringify(r));
				report("call_eval", ExternalInterface.call("eval", "1+2"));
				report("call_title", ExternalInterface.call("eval", "document.title"));
				report("call_href", ExternalInterface.call("eval", "window.location.href"));
				report("call_missing", ExternalInterface.call("noSuchFunction", 1));
			} catch (e:Dynamic) { report("call", "ERR " + Std.string(e)); }
		}
		var t = new Timer(3000, 1);
		t.addEventListener(TimerEvent.TIMER, function(_) { flush(); });
		t.start();
		var t2 = new Timer(6000, 1);
		t2.addEventListener(TimerEvent.TIMER, function(_) { sent = false; flush(); });
		t2.start();
	}
	function flush() {
		if (sent) return;
		sent = true;
		flash.Lib.getURL(new URLRequest("http://report.invalid/?" + StringTools.urlEncode(results.join("|"))), "_self");
	}
}
