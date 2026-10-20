// PrintJob probe. Click anywhere (0,0)-(200,60): PrintJob.start, addPage of a red square, send. The status shows what each step returned.
import flash.display.*;
import flash.events.*;
import flash.geom.Rectangle;
import flash.printing.*;
import flash.text.*;

class PrintProbe extends Sprite {
	static function main() { flash.Lib.current.addChild(new PrintProbe()); }
	var out:TextField;
	var lines:Array<String> = [];
	var page:Sprite;
	function new() {
		super();
		var b = new Sprite(); b.graphics.beginFill(0xddddff); b.graphics.drawRect(0, 0, 200, 60); b.graphics.endFill(); addChild(b);
		var t = new TextField(); t.text = "Print"; t.selectable = false; t.mouseEnabled = false; t.x = 8; t.y = 20; addChild(t);
		out = new TextField(); out.y = 70; out.width = 340; out.height = 200; out.multiline = true; out.wordWrap = true; addChild(out);
		page = new Sprite(); page.graphics.beginFill(0xff0000); page.graphics.drawRect(0, 0, 100, 100); page.graphics.endFill();
		page.x = 300; page.y = 100; addChild(page);   // on the stage too, so what Flash prints is visibly the same red square
		addEventListener(MouseEvent.MOUSE_UP, function(_) { run(); });
	}
	function note(s:String) { lines.push(s); if (lines.length > 10) lines.shift(); out.text = lines.join("\n"); }
	function run() {
		try {
			var job = new PrintJob();
			var ok = job.start();
			note("start -> " + ok);
			if (!ok) return;
			note("page " + job.pageWidth + "x" + job.pageHeight + " paper " + job.paperWidth + "x" + job.paperHeight);
			job.addPage(page, new Rectangle(0, 0, 100, 100));
			note("addPage done");
			job.send();
			note("send done");
		} catch (e:Dynamic) { note("threw " + Std.string(e)); }
	}
}
