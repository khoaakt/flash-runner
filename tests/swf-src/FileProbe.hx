// File dialog probe. Click "Open" (0,0)-(200,60): FileReference.browse, then load; PASS (green square at 350,250) when the loaded text is
// "flash-runner-file-test". Click "Save" (0,70)-(200,130): FileReference.save of a fixed text; the status line shows its events.
import flash.display.*;
import flash.events.*;
import flash.net.*;
import flash.text.*;

class FileProbe extends Sprite {
	static function main() { flash.Lib.current.addChild(new FileProbe()); }
	var out:TextField;
	var lines:Array<String> = [];
	var openRef:FileReference;
	var saveRef:FileReference;
	function new() {
		super();
		button(0, "Open"); button(70, "Save");
		out = new TextField(); out.x = 0; out.y = 140; out.width = 340; out.height = 160; out.multiline = true; out.wordWrap = true; addChild(out);
		addEventListener(MouseEvent.MOUSE_UP, function(e:MouseEvent) {
			if (e.localY >= 0 && e.localY < 60 && e.localX < 200) open();
			else if (e.localY >= 70 && e.localY < 130 && e.localX < 200) save();
		});
	}
	function button(y:Float, label:String) {
		var b = new Sprite(); b.graphics.beginFill(0xddddff); b.graphics.drawRect(0, y, 200, 60); b.graphics.endFill(); addChild(b);
		var t = new TextField(); t.text = label; t.selectable = false; t.mouseEnabled = false; t.x = 8; t.y = y + 20; addChild(t);
	}
	function note(s:String) { lines.push(s); if (lines.length > 8) lines.shift(); out.text = lines.join("\n"); }
	function open() {
		openRef = new FileReference();
		openRef.addEventListener(Event.SELECT, function(_) { note("select " + openRef.name + " size=" + openRef.size); openRef.load(); });
		openRef.addEventListener(Event.CANCEL, function(_) { note("cancel"); });
		openRef.addEventListener(Event.COMPLETE, function(_) {
			var s = openRef.data.toString(); note("loaded " + openRef.data.length + " bytes: " + s);
			if (s == "flash-runner-file-test") { graphics.beginFill(0x00ff00); graphics.drawRect(350, 250, 40, 40); graphics.endFill(); }
		});
		openRef.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) { note("io error " + e.text); });
		openRef.addEventListener(SecurityErrorEvent.SECURITY_ERROR, function(e:SecurityErrorEvent) { note("security error " + e.text); });
		try { var shown = openRef.browse([new FileFilter("Text", "*.txt")]); note("browse -> " + shown); } catch (e:Dynamic) { note("browse threw " + Std.string(e)); }
	}
	function save() {
		saveRef = new FileReference();
		saveRef.addEventListener(Event.SELECT, function(_) { note("save select " + saveRef.name); });
		saveRef.addEventListener(Event.CANCEL, function(_) { note("save cancel"); });
		saveRef.addEventListener(Event.COMPLETE, function(_) { note("save complete"); });
		saveRef.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) { note("save io error " + e.text); });
		try { saveRef.save("saved by flash-runner", "out.txt"); note("save called"); } catch (e:Dynamic) { note("save threw " + Std.string(e)); }
	}
}
