// navigateToURL("javascript:...") probe: after a second the movie asks the page to run a javascript: link; the page records the call.
import flash.display.*;
import flash.events.*;
import flash.net.*;
import flash.utils.Timer;

class JsLink extends Sprite {
	static function main() { flash.Lib.current.addChild(new JsLink()); }
	function new() {
		super();
		var t = new Timer(1000, 1);
		t.addEventListener(TimerEvent.TIMER, function(_) {
			flash.Lib.getURL(new URLRequest("javascript:jsLink('from flash', 42, true); void(0)"), "_self");
		});
		t.start();
	}
}
