// Compatibility probe movie (AS3 via the Haxe compiler — no Adobe SDK involved). It exercises the parts of the
// player that real games rely on and reports each result through navigateToURL, which the host observes
// (PPB_Flash::Navigate), so tests can assert on it without any JavaScript bridge.
import flash.display.*;
import flash.events.*;
import flash.net.*;
import flash.media.*;
import flash.text.*;
import flash.utils.*;
import flash.system.*;

class Compat extends Sprite {
	var out:TextField;
	var square:Sprite;
	var frames = 0;
	var sampleEvents = 0;
	var clicks = 0;

	static function main() {
		flash.Lib.current.addChild(new Compat());
	}

	function new() {
		super();
		addEventListener(Event.ADDED_TO_STAGE, init);
	}

	var results:Array<String> = [];
	var sent = false;
	// Results are shown on screen and also batched into one navigateToURL (the test host's side channel), because
	// Flash coalesces several navigations issued in the same frame.
	function report(k:String, v:String) {
		out.appendText(k + "=" + v + "\n");
		results.push(k + "=" + v);
	}
	function flushReport() {
		if (sent) return;
		sent = true;
		try {
			flash.Lib.getURL(new URLRequest("http://report.invalid/?" + StringTools.urlEncode(results.join("|"))), "_self");
		} catch (e:Dynamic) {}
	}

	function init(_) {
		stage.scaleMode = StageScaleMode.NO_SCALE;
		stage.align = StageAlign.TOP_LEFT;
		// device-font text field: needs PPB_Flash font support to be visible
		out = new TextField();
		out.defaultTextFormat = new TextFormat("_sans", 11, 0x202020);
		out.x = 6; out.y = 6; out.width = 300; out.height = 300; out.multiline = true; out.wordWrap = true;
		addChild(out);

		square = new Sprite();
		square.graphics.beginFill(0xcc3300);
		square.graphics.drawRect(-15, -15, 30, 30);
		square.graphics.endFill();
		square.x = 360; square.y = 60;
		addChild(square);

		report("version", Capabilities.version);
		report("os", Capabilities.os + "/" + Capabilities.cpuArchitecture);
		report("player", Capabilities.playerType);
		report("stage", stage.stageWidth + "x" + stage.stageHeight);
		report("flashvars", haxe.Json.stringify(flash.Lib.current.loaderInfo.parameters));
		report("date", Std.string(Date.now().getFullYear()));

		addEventListener(Event.ENTER_FRAME, function(_) {
			frames++;
			square.rotation += 3;
		});
		var t0 = Lib_getTimer();
		var tm = new Timer(3000, 1);
		tm.addEventListener(TimerEvent.TIMER, function(_) {
			report("frames3s", Std.string(frames));
			report("samples3s", Std.string(sampleEvents));
			flushReport();
		});
		tm.start();

		stage.addEventListener(MouseEvent.MOUSE_DOWN, function(e:MouseEvent) {
			clicks++;
			square.x = e.stageX; square.y = e.stageY;
			report("click", Std.int(e.stageX) + "," + Std.int(e.stageY));
		});
		stage.addEventListener(KeyboardEvent.KEY_DOWN, function(e:KeyboardEvent) {
			report("key", e.keyCode + "/" + e.charCode);
		});

		// shared object persistence
		var so = SharedObject.getLocal("compat");
		var runs:Int = so.data.runs == null ? 0 : so.data.runs;
		so.data.runs = runs + 1;
		so.flush();
		report("sharedobject_runs", Std.string(runs + 1));

		// network through the page: fetch a sibling resource
		var u = new URLLoader();
		u.addEventListener(Event.COMPLETE, function(_) report("urlloader", "ok:" + Std.string(u.data).length));
		u.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) report("urlloader", "error:" + e.text));
		var base = flash.Lib.current.loaderInfo.url;
		u.load(new URLRequest(base.substr(0, base.lastIndexOf("/") + 1) + "data.txt"));

		// bitmap work (software rasteriser + filters)
		var bd = new BitmapData(256, 256, false, 0xffffff);
		var s0 = Lib_getTimer();
		bd.noise(7, 0, 255, 7, true);
		for (i in 0...30) bd.applyFilter(bd, bd.rect, new flash.geom.Point(0, 0), new flash.filters.BlurFilter(4, 4, 1));
		report("bitmap_ms", Std.string(Lib_getTimer() - s0));
		var bm = new Bitmap(bd); bm.x = 320; bm.y = 100; bm.scaleX = bm.scaleY = 0.6; addChild(bm);

		// audio: generate a tone with sampleData
		var snd = new Sound();
		var phase = 0.0;
		snd.addEventListener(SampleDataEvent.SAMPLE_DATA, function(e:SampleDataEvent) {
			sampleEvents++;
			for (i in 0...2048) {
				var v = Math.sin(phase) * 0.2; phase += 2 * Math.PI * 440 / 44100;
				e.data.writeFloat(v); e.data.writeFloat(v);
			}
		});
		var ch = snd.play();
		report("sound_channel", ch != null ? "ok" : "null");
	}

	static function Lib_getTimer():Int return flash.Lib.getTimer();
}
