// Camera and microphone probe. Click Start (0,0)-(200,60). Shows what Camera / Microphone report, live; paints PASS (green square at 350,250) once
// the camera has delivered a frame that is not one flat colour AND the microphone has reported some activity (the test browsers use fake devices:
// a moving test pattern and a periodic beep). Flash asks for permission with its own dialog the first time: the test clicks its Allow button.
import flash.display.*;
import flash.events.*;
import flash.media.*;
import flash.text.*;

class CamProbe extends Sprite {
	static function main() { flash.Lib.current.addChild(new CamProbe()); }
	var out:TextField;
	var cam:Camera;
	var mic:Microphone;
	var video:Video;
	var bd:BitmapData;
	var frameOk = false;
	var micOk = false;
	var started = false;
	var passed = false;
	var peak = 0.0;
	var sampleEvents = 0;
	var head = "";
	var events:Array<String> = [];
	function new() {
		super();
		var b = new Sprite(); b.graphics.beginFill(0xddddff); b.graphics.drawRect(0, 0, 200, 60); b.graphics.endFill(); addChild(b);
		var t = new TextField(); t.text = "Start"; t.selectable = false; t.mouseEnabled = false; t.x = 8; t.y = 20; addChild(t);
		video = new Video(160, 120); video.y = 70; addChild(video);
		out = new TextField(); out.x = 170; out.y = 70; out.width = 330; out.height = 200; out.multiline = true; out.wordWrap = true; addChild(out);
		bd = new BitmapData(160, 120, false, 0);
		addEventListener(MouseEvent.MOUSE_UP, function(_) { if (!started) start(); });
		addEventListener(Event.ENTER_FRAME, tick);
	}
	function note(x:String) { events.push(x); if (events.length > 4) events.shift(); }
	function start() {
		started = true;
		var names = Camera.names; var mnames = Microphone.names;
		head = "cameras: " + names.length + " [" + names.join(",") + "]\nmics: " + mnames.length;
		cam = Camera.getCamera();
		if (cam != null) {
			cam.setMode(160, 120, 15); video.attachCamera(cam);
			cam.addEventListener(StatusEvent.STATUS, function(e:StatusEvent) { note("camera " + e.code); });
			head += "\ncamera: " + cam.name;
		} else head += "\ncamera: null";
		mic = Microphone.getMicrophone();
		if (mic != null) {
			mic.rate = 44;
			mic.addEventListener(SampleDataEvent.SAMPLE_DATA, function(e:SampleDataEvent) {      // listening for samples is what makes Flash start capturing
				sampleEvents++;
				while (e.data.bytesAvailable >= 4) { var v = Math.abs(e.data.readFloat()); if (v > peak) peak = v; }
			});
			mic.addEventListener(StatusEvent.STATUS, function(e:StatusEvent) { note("mic " + e.code); });
			head += "\nmic: " + mic.name;
		} else head += "\nmic: null";
	}
	function tick(_) {
		if (!started) return;
		if (cam != null && !frameOk && !cam.muted) {
			bd.draw(video);
			var first = bd.getPixel(2, 2); var diff = false;
			for (x in 0...40) for (y in 0...30) if (bd.getPixel(x * 4, y * 4) != first) diff = true;
			if (diff) frameOk = true;
		}
		if (peak > 0.01) micOk = true;     // silence is exactly 0; the browsers' fake tones differ in loudness
		out.text = head + "\n" + events.join("\n") + "\nframe=" + frameOk + " mic=" + micOk + " fps=" + (cam != null ? cam.currentFPS : -1) + " act=" + (mic != null ? mic.activityLevel : -1) + " samples=" + sampleEvents + " peak=" + Math.round(peak * 100) / 100
			+ " camMuted=" + (cam != null ? cam.muted : null) + " micMuted=" + (mic != null ? mic.muted : null);
		if (!passed && frameOk && micOk) { passed = true; graphics.beginFill(0x00ff00); graphics.drawRect(350, 250, 40, 40); graphics.endFill(); }
	}
}
