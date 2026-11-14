<?php

declare(strict_types=1);

namespace Runner;

use Proof\Box;
use Proof\Factory;
use Proof\Shop\Crate;
use Proof\Shop\Feed;
use Proof\Shop\Label;
use Proof\Shop\Ledger;
use Proof\Shop\Note;
use Proof\Shop\Order;
use Proof\Shop\Pair;
use Proof\Shop\Shelf;
use Proof\Shop\Stock;
use Proof\Shop\Tagged;
use Proof\Shop\Tally;
use Proof\Spec;
use Proof\Special;
use Proof\Zoo\Bird;
use Proof\Zoo\Cat;

require __DIR__ . '/vendor/autoload.php';

/** @return \Generator<string, string> */
function keyed(): \Generator
{
    yield 'first' => 'a';
    yield 'second' => 'b';
}

final class CountedSpec extends Spec implements \Countable
{
    public function count(): int
    {
        return 2;
    }
}

// An untyped parameter takes the `array|Sharp\Map` a PHP# `Map` parameter erases to.
final class ArraySource implements \Proof\Shop\Source
{
    /** @param array<string, int> $limits */
    public function limited($limits = []): int
    {
        return count($limits);
    }

    /** @param array<array-key, mixed> $options */
    public function read($options = []): int
    {
        return count($options);
    }
}

// A plain PHP subclass whose `new static(…)` makes a gift coupon.
final class GiftCoupon extends \Proof\Shop\Coupon
{
}

// Calls each class of the fixture from plain PHP in another namespace, before and after the conversion.
$spec = new Spec('alpha');
$special = new Special('beta');
echo $spec->label(), "\n";
echo $special->label(), "\n";
echo $spec->name, "\n";
echo json_encode([$spec->sameKind($special), $special->sameKind($spec), $spec->sameKind(new \stdClass())]), "\n";

/** @var Box<Special> $box */
$box = new Box($special);
echo $box->get()->label(), "\n";

/** @var Shelf<Spec> $shelf */
$shelf = new Shelf();
$shelf->add($spec, 7);
$shelf->add($special, 9);
echo $shelf->size(), ' ', count($shelf->all()), ' ', implode(',', array_keys($shelf->numbered())), "\n";
foreach ($shelf->all() as $item) {
    echo $item->label(), "\n";
}
echo $shelf->report(), "\n";

$order = new Order($spec, 'R-1');
$order->add(2);
$order->add(3);
echo $order->summary(), ' ', $order->reference, "\n";
echo $order->isFor('R-1') ? 'for R-1' : 'not for R-1', ' ', $order->isOf($special) ? 'of beta' : 'not of beta', ' ', $order->loud(), "\n";
echo $order->labelled(new CountedSpec('omega')), ' ', $order->same(Spec::class), "\n";
echo $order->found('0'), ' ', $order->found(''), ' ', $order->found(false), ' ', $order->found(true), "\n";

$crate = new Crate(new Spec('gamma'));
echo $crate->open()->label(), ' ', $crate->special()->get()->label(), "\n";

$tagged = new Tagged('delta', 'new');
echo $tagged->label(), ' ', $tagged->tag, ' ', $tagged->name, "\n";

$note = new Note();
echo $note->describe('hi'), ' / ', $note->describe(5), "\n";
echo $note->headed('memo'), ' ', $note->headed('0'), ' ', $note->headed(''), ' ', $note->headed(null), "\n";
echo $note->signed('Ann'), ' ', $note->signed(null), "\n";
echo $note->tagged('yes', 3, '!'), ' ', $note->tagged(1, 0, null), ' ', $note->tagged(true, -2, ''), "\n";
echo $note->labelled(new \Acme\Format\Sku('A1')), "\n";
echo json_encode([$note->namesSpec('Proof\Spec'), $note->namesSpec('proof\spec'), $note->namesSpec(null)]), "\n";
echo json_encode([$note->matched(Spec::class, Spec::class, 'id'), $note->matched(Spec::class, null, 7), $note->matched(Spec::class, Special::class, '7')]), "\n";
echo json_encode([$note->flagged(0, '0'), $note->flagged(2, ''), $note->flagged(-1, 'x'), $note->flagged(0, null)]), "\n";

echo (new Factory())->make()->get()->label(), "\n";
$source = new ArraySource();
echo $source->limited(), ' ', $source->limited(['a' => 1]), ' ', $source->read(), ' ', $source->read([1, 2]), "\n";
echo (new Pair())->both(), "\n";

$tally = new Tally();
$plusThree = $tally->adder(3);
echo $tally->sum(2, 5, 10), ' ', $plusThree(4), ' ', $tally->halved(10), "\n";
echo $tally->counted([3]), ' ', $tally->made(Pair::class), ' ', $tally->priced(['price' => 4, 'count' => 3]), "\n";
echo $tally->carried([3, -1, 4, -2]), ' ', $tally->guarded(3), ' ', $tally->guarded(0), "\n";
echo $tally->bounded(2), ' ', $tally->bounded(0), ' ', $tally->bounded(-1), "\n";
echo $tally->graded(20), ' ', $tally->graded(6), ' ', $tally->graded(2), ' ', $tally->bounds(), ' ', $tally->stepped(3), "\n";
$gift = (new GiftCoupon(5))->doubled();
echo $gift::class, ' ', $gift->cents, ' ', (new \Proof\Shop\Coupon(3))->doubled()::class, "\n";
echo $tally->stamped(new \Acme\Format\Stamp()), "\n";

$label = new Label();
echo $label->render('shelf', 3), "\n", $label->folder(), "\n", $label->traced(), ' ', $label->kind(), "\n";
echo $label->status(201), ' ', $label->status(404), ' ', $label->status(500), "\n";
echo $label->size(500, false), ' ', $label->size(5, true), ' ', $label->size(20, false), ' ', $label->size(1, false), "\n";
echo $label->titled('hey'), ' ', $label->counted(4), ' ', $label->sized(['small' => 2]);
foreach ([fn () => $label->titled(5), fn () => $label->counted('4')] as $refused) {
    try {
        $refused();
    } catch (\TypeError $error) {
        echo ' ', $error::class;
    }
}
echo "\n";

// A PHPDoc `int` that holds `"5"`, a `string` key PHP reads as `int`, and generator keys through a PHP# parameter.
$stock = new Stock();
echo var_export($stock->quantity(new \Acme\Format\Record(['quantity' => '5'])), true), ' ', $stock->joined(keyed()), "\n";
echo var_export($stock->coded(new \Acme\Format\Record(['code' => 5])), true), "\n";
echo $stock->counted(new \Acme\Format\Lines(['a', 'b', 'c'])), ' ', implode(',', $stock->marked(new \Acme\Format\Lines(['a', 'b']))), "\n";
echo (new Ledger())->priceOf(['42' => 1, 'x' => 2], '42'), "\n";
echo var_export($stock->given(['email' => 'a', 5 => 'x', '42' => 'y']), true), "\n";
echo var_export($stock->shelves(), true), "\n";
echo $stock->nth(['a', 'b'], 1), ' ', $stock->nth(['a'], 5), ' ', $stock->nth(['a'], -1), "\n";
$bag = new \Acme\Format\Bag();
echo $stock->stocked($bag, 'jam'), ' ', $stock->stocked($bag, 'tea'), ' ', $bag['jam'], ' ', $bag->removals, "\n";
echo $stock->listed(['42' => 1, 'x' => 2]), ' ', var_export($stock->kept(['42' => 1, 'x' => 2], ['42' => true]), true), "\n";
$through = (new Feed())->through(keyed());
foreach ($through as $key => $item) {
    echo var_export($key, true), '=', $item, ' ';
}
echo get_debug_type($through), "\n";

$cat = new Cat('tom');
$bird = new Bird('tweety');
echo $cat->describe(), "\n", $bird->describe(), "\n";
echo Cat::counted(1), ' ', Bird::counted(3), "\n";
echo $cat->summary(), ' ', $bird->summary(), ' ', json_encode($cat->traits()), "\n";
$bird->sound = 'chirp';
$bird->name = 'polly';
echo $bird->describe(), "\n";
