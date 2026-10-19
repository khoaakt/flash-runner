// Network probe: GET and POST /echo-ref on the movie's own origin and show what Referer / Origin the server saw.
import flash.display.*;
import flash.events.*;
import flash.net.*;
import flash.text.*;

class Net extends Sprite {
	var tf:TextField;
	static function main() { flash.Lib.current.addChild(new Net()); }
	function new() {
		super();
		tf = new TextField(); tf.defaultTextFormat = new TextFormat("_sans", 12, 0x202020); tf.x = 6; tf.y = 6; tf.width = 460; tf.height = 200; tf.multiline = true; tf.wordWrap = true;
		addChild(tf);
		addEventListener(Event.ADDED_TO_STAGE, function(_) {
			var u = flash.Lib.current.loaderInfo.url;
			var origin = u.substr(0, u.indexOf("/", 8));
			var cross = flash.Lib.current.loaderInfo.parameters.cross;     // flashvars cross=localhost: same server, different host => policy file needed
			if (cross != null) origin = StringTools.replace(origin, "127.0.0.1", cross);
			tf.appendText("target " + origin + "\n");
			get(origin + "/echo-ref", "GET");
			get(origin + "/echo-ref", "POST");
		});
	}
	function get(url:String, method:String) {
		var l = new URLLoader();
		var r = new URLRequest(url);
		if (method == "POST") { r.method = URLRequestMethod.POST; r.data = "a=1"; }
		l.addEventListener(Event.COMPLETE, function(_) tf.appendText(method + " -> " + Std.string(l.data) + "\n"));
		l.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) tf.appendText(method + " ERROR " + e.text + "\n"));
		l.addEventListener(SecurityErrorEvent.SECURITY_ERROR, function(e:SecurityErrorEvent) tf.appendText(method + " SECURITY " + e.text + "\n"));
		l.load(r);
	}
}
