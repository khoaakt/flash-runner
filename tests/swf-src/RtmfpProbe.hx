// RTMFP probe: NetConnection.connect("rtmfp://host:port/app") from flashvars `url`. RTMFP runs over UDP, which is why Flash opens UDP sockets; the status
// lines show every NetConnection event. PASS (green square at 350,250) only if the connection succeeds, which needs a real RTMFP server.
import flash.display.*;
import flash.events.*;
import flash.net.*;
import flash.text.*;

class RtmfpProbe extends Sprite {
	static function main() { flash.Lib.current.addChild(new RtmfpProbe()); }
	var out:TextField;
	var lines:Array<String> = [];
	function new() {
		super();
		var url:String = flash.Lib.current.loaderInfo.parameters.url;
		out = new TextField(); out.width = 480; out.height = 200; out.multiline = true; out.wordWrap = true; addChild(out);
		var nc = new NetConnection();
		nc.addEventListener(NetStatusEvent.NET_STATUS, function(e:NetStatusEvent) {
			note("nc " + e.info.code);
			if (e.info.code == "NetConnection.Connect.Success") { graphics.beginFill(0x00ff00); graphics.drawRect(350, 250, 40, 40); graphics.endFill(); }
		});
		nc.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) { note("io error " + e.text); });
		nc.addEventListener(SecurityErrorEvent.SECURITY_ERROR, function(e:SecurityErrorEvent) { note("security error " + e.text); });
		note("connecting to " + url);
		nc.connect(url);
	}
	function note(s:String) { lines.push(s); if (lines.length > 9) lines.shift(); out.text = lines.join("\n"); }
}
